use anyhow::{Context, bail, ensure};
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use url::Url;

/// The query parameter, and its value, that sends a server's connections through the cpxy server.
const VIA_PARAM: &str = "via";
const VIA_CPXY: &str = "cpxy";

/// How to reach a DNS server, as given on the command line.
///
/// Accepted forms:
/// - `8.8.8.8`, `8.8.8.8:53`, `udp://8.8.8.8:53`: plain DNS over UDP
/// - `tcp://8.8.8.8:53`: plain DNS over TCP
///
/// `tcp://` accepts `?via=cpxy` to connect through the cpxy server, so neither the query nor the
/// answer can be tampered with on the way.
///
/// DNS over TLS (`tls://`) and DNS over HTTPS (`https://`) are not supported: use
/// `tcp://<ip>?via=cpxy` for a resolver that must not be tampered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub endpoint: Endpoint,
    pub via: Via,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Udp(SocketAddr),
    Tcp(SocketAddr),
}

/// The way connections to a DNS server go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Via {
    #[default]
    Direct,
    /// Through the cpxy server, which carries TCP only.
    Cpxy,
}

impl ServerSpec {
    pub fn direct(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            via: Via::Direct,
        }
    }

    /// The server's address.
    pub fn addr(&self) -> SocketAddr {
        match self.endpoint {
            Endpoint::Udp(addr) | Endpoint::Tcp(addr) => addr,
        }
    }
}

fn parse_host_ip(url: &Url) -> anyhow::Result<IpAddr> {
    let host = url.host_str().context("Missing host")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.parse()
        .with_context(|| format!("{host} is not an IP address"))
}

fn parse_via_param(url: &Url) -> anyhow::Result<Via> {
    match url.query_pairs().find(|(k, _)| k == VIA_PARAM) {
        None => Ok(Via::Direct),
        Some((_, v)) if v == VIA_CPXY => Ok(Via::Cpxy),
        Some((_, v)) => bail!("Invalid {VIA_PARAM} parameter {v}, only {VIA_CPXY} is supported"),
    }
}

impl FromStr for ServerSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::direct(Endpoint::Udp(SocketAddr::new(ip, 53))));
        }

        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Self::direct(Endpoint::Udp(addr)));
        }

        let url = Url::parse(s).with_context(|| format!("Invalid DNS server spec {s}"))?;
        let endpoint = match url.scheme() {
            "udp" => Endpoint::Udp(SocketAddr::new(
                parse_host_ip(&url)?,
                url.port().unwrap_or(53),
            )),
            "tcp" => Endpoint::Tcp(SocketAddr::new(
                parse_host_ip(&url)?,
                url.port().unwrap_or(53),
            )),
            scheme @ ("tls" | "https") => bail!(
                "{s}: {scheme}:// DNS servers are no longer supported, use tcp://<ip>?via=cpxy instead"
            ),
            scheme => bail!("Unsupported DNS server scheme {scheme}"),
        };
        let via = parse_via_param(&url)?;
        ensure!(
            via == Via::Direct || matches!(endpoint, Endpoint::Tcp(_)),
            "{s}: the cpxy server carries TCP only, use tcp://"
        );
        Ok(Self { endpoint, via })
    }
}

impl Display for ServerSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.endpoint {
            Endpoint::Udp(addr) => write!(f, "udp://{addr}")?,
            Endpoint::Tcp(addr) => write!(f, "tcp://{addr}")?,
        }
        match self.via {
            Via::Direct => Ok(()),
            Via::Cpxy => write!(f, "?{VIA_PARAM}={VIA_CPXY}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_addresses() {
        assert_eq!(
            "8.8.8.8".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Udp("8.8.8.8:53".parse().unwrap()))
        );
        assert_eq!(
            "1.1.1.1:5353".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Udp("1.1.1.1:5353".parse().unwrap()))
        );
        assert_eq!(
            "udp://9.9.9.9".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Udp("9.9.9.9:53".parse().unwrap()))
        );
        assert_eq!(
            "tcp://9.9.9.9:5353".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Tcp("9.9.9.9:5353".parse().unwrap()))
        );
        assert_eq!(
            "tcp://[2001:db8::1]".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Tcp("[2001:db8::1]:53".parse().unwrap()))
        );
    }

    #[test]
    fn parses_via_cpxy() {
        let spec = "tcp://1.1.1.1?via=cpxy".parse::<ServerSpec>().unwrap();
        assert_eq!(
            spec,
            ServerSpec {
                endpoint: Endpoint::Tcp("1.1.1.1:53".parse().unwrap()),
                via: Via::Cpxy,
            }
        );
        assert_eq!(spec.to_string(), "tcp://1.1.1.1:53?via=cpxy");
    }

    #[test]
    fn rejects_encrypted_dns() {
        for spec in [
            "tls://1.1.1.1",
            "tls://dns.google?ip=8.8.4.4&via=cpxy",
            "https://dns.google/dns-query",
        ] {
            let error = spec.parse::<ServerSpec>().unwrap_err().to_string();
            assert!(error.contains("no longer supported"), "{spec}: {error}");
        }
    }

    #[test]
    fn rejects_invalid_specs() {
        assert!("udp://dns.google".parse::<ServerSpec>().is_err());
        assert!("tcp://dns.google".parse::<ServerSpec>().is_err());
        assert!("quic://dns.google".parse::<ServerSpec>().is_err());
        assert!("not a server".parse::<ServerSpec>().is_err());
        // The cpxy server carries TCP only
        assert!("udp://8.8.8.8?via=cpxy".parse::<ServerSpec>().is_err());
        assert!("tcp://8.8.8.8?via=socks".parse::<ServerSpec>().is_err());
    }
}
