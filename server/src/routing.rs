use anyhow::{Context, bail, ensure};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Where to send the upstream connection for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Direct,
    /// A SOCKS5 server (no authentication) given as `host:port`.
    Socks5(String),
}

impl Display for Route {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Route::Direct => f.write_str("direct"),
            Route::Socks5(addr) => write!(f, "socks5://{addr}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    /// `*`: matches every host.
    Any,
    /// `=host`: matches only this host.
    Exact(String),
    /// `host`: matches this host and any of its subdomains.
    Suffix(String),
}

impl Pattern {
    /// Returns the match specificity, or `None` if the host doesn't match.
    /// Exact matches beat suffix matches, longer suffixes beat shorter ones,
    /// and `*` loses to everything.
    fn specificity(&self, host: &str) -> Option<usize> {
        match self {
            Pattern::Any => Some(0),
            Pattern::Exact(h) => (h == host).then_some(usize::MAX),
            Pattern::Suffix(s) => {
                let matches = host == s
                    || host
                        .strip_suffix(s.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'));
                matches.then_some(s.len() + 1)
            }
        }
    }
}

/// A single `--socks5-route` rule: `<HOST_PATTERN>=<TARGET>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pattern: Pattern,
    route: Route,
}

impl FromStr for Rule {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // Split on the first `=` that isn't the leading exact-match marker.
        let split_at = s
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '=')
            .map(|(i, _)| i)
            .with_context(|| format!("Invalid route {s:?}: expected <HOST_PATTERN>=<TARGET>"))?;
        let (pattern, target) = (&s[..split_at], &s[split_at + 1..]);

        let pattern = match pattern.trim() {
            "*" => Pattern::Any,
            p => {
                let (exact, host) = match p.strip_prefix('=') {
                    Some(h) => (true, h),
                    None => (false, p),
                };
                let host = normalize_host(host);
                ensure!(!host.is_empty(), "Invalid route {s:?}: empty host pattern");
                if exact {
                    Pattern::Exact(host)
                } else {
                    Pattern::Suffix(host)
                }
            }
        };

        let target = target.trim();
        let route = if target.eq_ignore_ascii_case("direct") {
            Route::Direct
        } else if let Some(addr) = target.strip_prefix("socks5://") {
            let addr = addr.trim_end_matches('/');
            ensure!(
                !addr.contains('@'),
                "Invalid route {s:?}: SOCKS5 authentication is not supported"
            );
            let (host, port) = addr
                .rsplit_once(':')
                .with_context(|| format!("Invalid route {s:?}: SOCKS5 address needs a port"))?;
            ensure!(!host.is_empty(), "Invalid route {s:?}: empty SOCKS5 host");
            port.parse::<u16>()
                .with_context(|| format!("Invalid route {s:?}: bad SOCKS5 port"))?;
            Route::Socks5(addr.to_string())
        } else {
            bail!("Invalid route {s:?}: target must be `direct` or `socks5://host:port`");
        };

        Ok(Rule { pattern, route })
    }
}

/// Picks a [`Route`] for an incoming request based on its HTTP `Host` header.
#[derive(Debug, Default)]
pub struct Router {
    rules: Vec<Rule>,
}

impl Router {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// Returns the most specific matching route, or [`Route::Direct`] if none match.
    /// `host_header` is the raw `Host` header value (it may carry a port).
    pub fn route(&self, host_header: &str) -> &Route {
        static DIRECT: Route = Route::Direct;
        let host = normalize_host_header(host_header);
        self.rules
            .iter()
            .filter_map(|r| r.pattern.specificity(&host).map(|s| (s, &r.route)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, route)| route)
            .unwrap_or(&DIRECT)
    }
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Strips the port (and IPv6 brackets) from a `Host` header value and normalises it.
fn normalize_host_header(value: &str) -> String {
    let value = value.trim();
    let host = if let Some(rest) = value.strip_prefix('[') {
        rest.split_once(']').map_or(rest, |(h, _)| h)
    } else if value.matches(':').count() == 1 {
        value.split_once(':').map_or(value, |(h, _)| h)
    } else {
        value
    };
    normalize_host(host)
}

/// Opens a TCP connection to `host:port` following `route`.
pub async fn dial(route: &Route, host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let stream = match route {
        Route::Direct => TcpStream::connect((host, port))
            .await
            .context("Error connecting to upstream")?,
        Route::Socks5(proxy) => {
            let stream = TcpStream::connect(proxy.as_str())
                .await
                .with_context(|| format!("Error connecting to SOCKS5 server {proxy}"))?;
            socks5_connect(stream, host, port)
                .await
                .with_context(|| format!("SOCKS5 CONNECT via {proxy} failed"))?
        }
    };

    stream.set_nodelay(true).context("Error setting nodelay")?;
    Ok(stream)
}

/// Performs an unauthenticated SOCKS5 CONNECT, passing `host` through as a
/// domain name so the SOCKS5 server does the DNS resolution.
async fn socks5_connect(mut stream: TcpStream, host: &str, port: u16) -> anyhow::Result<TcpStream> {
    ensure!(host.len() <= 255, "Host name too long for SOCKS5");

    stream.write_all(&[5, 1, 0]).await?;
    let mut reply = [0u8; 2];
    stream
        .read_exact(&mut reply)
        .await
        .context("Reading method selection")?;
    ensure!(reply[0] == 5, "Unexpected SOCKS version {}", reply[0]);
    ensure!(reply[1] == 0, "SOCKS5 server requires authentication");

    let mut req = Vec::with_capacity(7 + host.len());
    req.extend_from_slice(&[5, 1, 0, 3, host.len() as u8]);
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).await?;

    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .context("Reading CONNECT reply")?;
    ensure!(head[0] == 5, "Unexpected SOCKS version {}", head[0]);
    ensure!(
        head[1] == 0,
        "SOCKS5 CONNECT rejected with code {}",
        head[1]
    );

    // Skip the bound address and port.
    let addr_len = match head[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        t => bail!("Unknown SOCKS5 address type {t}"),
    };
    let mut skip = vec![0u8; addr_len + 2];
    stream.read_exact(&mut skip).await?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router(rules: &[&str]) -> Router {
        Router::new(rules.iter().map(|r| r.parse().unwrap()).collect())
    }

    fn socks(addr: &str) -> Route {
        Route::Socks5(addr.to_string())
    }

    #[test]
    fn parses_rules() {
        assert_eq!(
            "example.com=socks5://10.0.0.1:1080"
                .parse::<Rule>()
                .unwrap(),
            Rule {
                pattern: Pattern::Suffix("example.com".into()),
                route: socks("10.0.0.1:1080"),
            }
        );
        assert_eq!(
            "=Example.COM.=direct".parse::<Rule>().unwrap(),
            Rule {
                pattern: Pattern::Exact("example.com".into()),
                route: Route::Direct,
            }
        );
        assert_eq!(
            "*=socks5://[::1]:1080".parse::<Rule>().unwrap().route,
            socks("[::1]:1080")
        );

        for bad in [
            "example.com",
            "=example.com",
            "=socks5://a:1",
            "example.com=http://a:1",
            "example.com=socks5://a",
            "example.com=socks5://a:notaport",
            "example.com=socks5://user:pw@a:1",
        ] {
            assert!(bad.parse::<Rule>().is_err(), "{bad} should fail to parse");
        }
    }

    #[test]
    fn normalizes_host_header() {
        assert_eq!(normalize_host_header("Example.com:3000"), "example.com");
        assert_eq!(normalize_host_header("example.com."), "example.com");
        assert_eq!(normalize_host_header("[::1]:3000"), "::1");
        assert_eq!(normalize_host_header("::1"), "::1");
        assert_eq!(normalize_host_header(""), "");
    }

    #[test]
    fn picks_most_specific_route() {
        let r = router(&[
            "*=socks5://any:1",
            "example.com=socks5://suffix:1",
            "corp.example.com=direct",
            "=exact.corp.example.com=socks5://exact:1",
        ]);

        assert_eq!(r.route("example.com"), &socks("suffix:1"));
        assert_eq!(r.route("a.example.com:443"), &socks("suffix:1"));
        assert_eq!(r.route("corp.example.com"), &Route::Direct);
        assert_eq!(r.route("x.corp.example.com"), &Route::Direct);
        assert_eq!(r.route("EXACT.corp.example.com"), &socks("exact:1"));
        assert_eq!(r.route("badexample.com"), &socks("any:1"));
        assert_eq!(r.route(""), &socks("any:1"));
    }

    #[test]
    fn defaults_to_direct() {
        assert_eq!(Router::default().route("example.com"), &Route::Direct);
        assert_eq!(
            router(&["example.com=socks5://a:1"]).route("other.com"),
            &Route::Direct
        );
    }
}
