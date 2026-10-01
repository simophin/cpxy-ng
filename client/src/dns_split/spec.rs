use anyhow::{Context, bail};
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use url::Url;

/// The default DoH endpoint path when a `https://` spec has no path.
const DEFAULT_DOH_PATH: &str = "/dns-query";

/// How to reach a DNS server, as given on the command line.
///
/// Accepted forms:
/// - `8.8.8.8`, `8.8.8.8:53`, `udp://8.8.8.8:53`: plain DNS over UDP
/// - `tcp://8.8.8.8:53`: plain DNS over TCP
/// - `tls://dns.google:853`: DNS over TLS
/// - `https://dns.google/dns-query`: DNS over HTTPS
///
/// `tls://` and `https://` accept `?ip=<addr>` to skip the startup hostname lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSpec {
    Udp(SocketAddr),
    Tcp(SocketAddr),
    Tls {
        host: String,
        port: u16,
        ip: Option<IpAddr>,
    },
    Https {
        host: String,
        port: u16,
        path: String,
        ip: Option<IpAddr>,
    },
}

impl ServerSpec {
    /// The address to connect to, looking up the hostname with the OS resolver if needed.
    pub async fn resolve_addr(&self) -> anyhow::Result<SocketAddr> {
        let (host, port, ip) = match self {
            Self::Udp(addr) | Self::Tcp(addr) => return Ok(*addr),
            Self::Tls { host, port, ip } | Self::Https { host, port, ip, .. } => (host, *port, ip),
        };

        if let Some(ip) = ip {
            return Ok(SocketAddr::new(*ip, port));
        }

        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(SocketAddr::new(ip, port));
        }

        tokio::net::lookup_host((host.as_str(), port))
            .await
            .with_context(|| format!("Error looking up DNS server {host}"))?
            .next()
            .with_context(|| format!("No address found for DNS server {host}"))
    }
}

fn parse_host(url: &Url) -> anyhow::Result<String> {
    let host = url.host_str().context("Missing host")?;
    Ok(host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string())
}

fn parse_host_ip(url: &Url) -> anyhow::Result<IpAddr> {
    let host = parse_host(url)?;
    host.parse()
        .with_context(|| format!("{host} is not an IP address"))
}

fn parse_ip_param(url: &Url) -> anyhow::Result<Option<IpAddr>> {
    url.query_pairs()
        .find(|(k, _)| k == "ip")
        .map(|(_, v)| {
            v.parse()
                .with_context(|| format!("Invalid ip parameter {v}"))
        })
        .transpose()
}

impl FromStr for ServerSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::Udp(SocketAddr::new(ip, 53)));
        }

        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Self::Udp(addr));
        }

        let url = Url::parse(s).with_context(|| format!("Invalid DNS server spec {s}"))?;
        match url.scheme() {
            "udp" => Ok(Self::Udp(SocketAddr::new(
                parse_host_ip(&url)?,
                url.port().unwrap_or(53),
            ))),
            "tcp" => Ok(Self::Tcp(SocketAddr::new(
                parse_host_ip(&url)?,
                url.port().unwrap_or(53),
            ))),
            "tls" => Ok(Self::Tls {
                host: parse_host(&url)?,
                port: url.port().unwrap_or(853),
                ip: parse_ip_param(&url)?,
            }),
            "https" => Ok(Self::Https {
                host: parse_host(&url)?,
                port: url.port_or_known_default().unwrap_or(443),
                path: match url.path() {
                    "" | "/" => DEFAULT_DOH_PATH.to_string(),
                    p => p.to_string(),
                },
                ip: parse_ip_param(&url)?,
            }),
            scheme => bail!("Unsupported DNS server scheme {scheme}"),
        }
    }
}

impl Display for ServerSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Udp(addr) => write!(f, "udp://{addr}"),
            Self::Tcp(addr) => write!(f, "tcp://{addr}"),
            Self::Tls { host, port, .. } => write!(f, "tls://{host}:{port}"),
            Self::Https {
                host, port, path, ..
            } => write!(f, "https://{host}:{port}{path}"),
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
            ServerSpec::Udp("8.8.8.8:53".parse().unwrap())
        );
        assert_eq!(
            "1.1.1.1:5353".parse::<ServerSpec>().unwrap(),
            ServerSpec::Udp("1.1.1.1:5353".parse().unwrap())
        );
        assert_eq!(
            "udp://9.9.9.9".parse::<ServerSpec>().unwrap(),
            ServerSpec::Udp("9.9.9.9:53".parse().unwrap())
        );
        assert_eq!(
            "tcp://9.9.9.9:5353".parse::<ServerSpec>().unwrap(),
            ServerSpec::Tcp("9.9.9.9:5353".parse().unwrap())
        );
        assert_eq!(
            "tcp://[2001:db8::1]".parse::<ServerSpec>().unwrap(),
            ServerSpec::Tcp("[2001:db8::1]:53".parse().unwrap())
        );
    }

    #[test]
    fn parses_tls() {
        assert_eq!(
            "tls://dns.google".parse::<ServerSpec>().unwrap(),
            ServerSpec::Tls {
                host: "dns.google".to_string(),
                port: 853,
                ip: None,
            }
        );
        assert_eq!(
            "tls://dns.google:8853?ip=8.8.4.4"
                .parse::<ServerSpec>()
                .unwrap(),
            ServerSpec::Tls {
                host: "dns.google".to_string(),
                port: 8853,
                ip: Some("8.8.4.4".parse().unwrap()),
            }
        );
    }

    #[test]
    fn parses_https() {
        assert_eq!(
            "https://dns.google".parse::<ServerSpec>().unwrap(),
            ServerSpec::Https {
                host: "dns.google".to_string(),
                port: 443,
                path: "/dns-query".to_string(),
                ip: None,
            }
        );
        assert_eq!(
            "https://cloudflare-dns.com:8443/custom?ip=1.1.1.1"
                .parse::<ServerSpec>()
                .unwrap(),
            ServerSpec::Https {
                host: "cloudflare-dns.com".to_string(),
                port: 8443,
                path: "/custom".to_string(),
                ip: Some("1.1.1.1".parse().unwrap()),
            }
        );
    }

    #[test]
    fn rejects_invalid_specs() {
        assert!("udp://dns.google".parse::<ServerSpec>().is_err());
        assert!("quic://dns.google".parse::<ServerSpec>().is_err());
        assert!("tls://dns.google?ip=nope".parse::<ServerSpec>().is_err());
        assert!("not a server".parse::<ServerSpec>().is_err());
    }
}
