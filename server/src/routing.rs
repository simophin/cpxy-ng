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

/// Matches a whole host against a pattern where each `*` matches any run of
/// characters (including none, and including dots). Both are expected lowercase.
fn wildcard_match(pattern: &str, host: &str) -> bool {
    let mut parts = pattern.split('*');
    // `split` always yields at least one part.
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = host.strip_prefix(first) else {
        return false;
    };

    let mut parts = parts.peekable();
    if parts.peek().is_none() {
        // No `*` at all: exact match.
        return rest.is_empty();
    }

    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            // The last part must end the host.
            return rest.ends_with(part);
        }
        // Middle parts: take the earliest occurrence, leaving the most room for the rest.
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    unreachable!("the loop returns on the last part")
}

/// A single `--socks5-route` rule: `<HOST_PATTERN>=<TARGET>`.
///
/// The pattern is case-insensitive and must match the whole `Host` header value
/// (with any port and trailing dot stripped); each `*` in it matches any run of
/// characters, including dots.
#[derive(Debug, Clone)]
pub struct Rule {
    pattern: String,
    route: Route,
}

impl Display for Rule {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} => {}", self.pattern, self.route)
    }
}

impl FromStr for Rule {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (pattern, target) = s
            .split_once('=')
            .with_context(|| format!("Invalid route {s:?}: expected <HOST_PATTERN>=<TARGET>"))?;

        let pattern = normalize_host(pattern);
        ensure!(
            !pattern.is_empty(),
            "Invalid route {s:?}: empty host pattern"
        );
        ensure!(
            !pattern.contains(|c: char| c.is_whitespace() || c == ','),
            "Invalid route {s:?}: host pattern can't contain whitespace or commas"
        );

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

/// A list of [`Rule`]s separated by commas and/or whitespace, as given in `SOCKS5_ROUTES`.
#[derive(Debug, Clone, Default)]
pub struct RuleList(pub Vec<Rule>);

impl FromStr for RuleList {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split(|c: char| c.is_whitespace() || c == ',')
            .filter(|r| !r.is_empty())
            .map(Rule::from_str)
            .collect::<Result<_, _>>()
            .map(RuleList)
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

    /// Returns the route of the first rule whose pattern matches, or [`Route::Direct`]
    /// if none do. `host_header` is the raw `Host` header value (it may carry a port).
    pub fn route(&self, host_header: &str) -> &Route {
        static DIRECT: Route = Route::Direct;
        let host = normalize_host_header(host_header);
        self.rules
            .iter()
            .find(|r| wildcard_match(&r.pattern, &host))
            .map_or(&DIRECT, |r| &r.route)
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
///
/// The greeting and CONNECT request are pipelined in one write (we only offer
/// "no auth", so the method reply is known in advance), making the handshake a
/// single round trip.
async fn socks5_connect(mut stream: TcpStream, host: &str, port: u16) -> anyhow::Result<TcpStream> {
    ensure!(host.len() <= 255, "Host name too long for SOCKS5");

    let mut req = Vec::with_capacity(10 + host.len());
    req.extend_from_slice(&[5, 1, 0]);
    req.extend_from_slice(&[5, 1, 0, 3, host.len() as u8]);
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).await?;

    let mut reply = [0u8; 2];
    stream
        .read_exact(&mut reply)
        .await
        .context("Reading method selection")?;
    ensure!(reply[0] == 5, "Unexpected SOCKS version {}", reply[0]);
    ensure!(reply[1] == 0, "SOCKS5 server requires authentication");

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
        let rule: Rule = "*.Example.COM.=socks5://10.0.0.1:1080".parse().unwrap();
        assert_eq!(rule.pattern, "*.example.com");
        assert_eq!(rule.route, socks("10.0.0.1:1080"));
        assert_eq!(
            "*=socks5://[::1]:1080".parse::<Rule>().unwrap().route,
            socks("[::1]:1080")
        );

        for bad in [
            "example.com",
            "=direct",
            "a=b=direct",
            "example.com=http://a:1",
            "example.com=socks5://a",
            "example.com=socks5://a:notaport",
            "example.com=socks5://user:pw@a:1",
        ] {
            assert!(bad.parse::<Rule>().is_err(), "{bad} should fail to parse");
        }
    }

    #[test]
    fn parses_rule_lists() {
        let RuleList(rules) = " a.com=direct,b.com=direct ,\n\t*.c.com=socks5://h:1, "
            .parse()
            .unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[2].route, socks("h:1"));
        assert!("".parse::<RuleList>().unwrap().0.is_empty());
        assert!("a.com=direct bad".parse::<RuleList>().is_err());
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
    fn wildcard_matching() {
        for (pattern, host) in [
            ("example.com", "example.com"),
            ("*", ""),
            ("*", "anything.at.all"),
            ("*.example.com", "a.example.com"),
            ("*.example.com", "a.b.example.com"),
            ("us*.proxy.*.net", "us1.proxy.hk.net"),
            ("us*.proxy.*.net", "us.proxy.a.b.net"),
            ("*.us.*", "a.us.b"),
            ("a*b*c", "abc"),
            ("a*b*c", "aXbYbZc"),
            ("**.com", "x.com"),
        ] {
            assert!(
                wildcard_match(pattern, host),
                "{pattern} should match {host}"
            );
        }

        for (pattern, host) in [
            ("example.com", "a.example.com"),
            ("example.com", "example.co"),
            ("*.example.com", "example.com"),
            ("*.example.com", "badexample.com"),
            ("*.example.com", "a.example.com.evil.net"),
            ("us*.proxy.*.net", "us1.proxy.net"),
            ("a*a", "a"),
            ("a*b*c", "acb"),
        ] {
            assert!(
                !wildcard_match(pattern, host),
                "{pattern} shouldn't match {host}"
            );
        }
    }

    #[test]
    fn matches_whole_host_and_first_rule_wins() {
        let r = router(&[
            "corp.example.com=direct",
            "example.com=socks5://apex:1",
            "*.example.com=socks5://sub:1",
            "us*.proxy.*.net=socks5://us:1",
            "*=socks5://any:1",
        ]);

        assert_eq!(r.route("example.com"), &socks("apex:1"));
        assert_eq!(r.route("A.Example.com:443"), &socks("sub:1"));
        assert_eq!(r.route("corp.example.com"), &Route::Direct);
        // The earlier rule only matches that exact name, so subdomains fall through.
        assert_eq!(r.route("x.corp.example.com"), &socks("sub:1"));
        assert_eq!(r.route("us12.proxy.hk.net"), &socks("us:1"));
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
