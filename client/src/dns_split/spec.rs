use anyhow::{Context, bail, ensure};
use std::fmt::{Display, Formatter};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use url::Url;

/// The default DoH endpoint path when a `https://` spec has no path.
const DEFAULT_DOH_PATH: &str = "/dns-query";
/// The query parameter, and its value, that sends a server's connections through the cpxy server.
const VIA_PARAM: &str = "via";
const VIA_CPXY: &str = "cpxy";

/// How to reach a DNS server, as given on the command line.
///
/// Accepted forms:
/// - `8.8.8.8`, `8.8.8.8:53`, `udp://8.8.8.8:53`: plain DNS over UDP
/// - `tcp://8.8.8.8:53`: plain DNS over TCP
/// - `tls://dns.google:853`: DNS over TLS
/// - `https://dns.google/dns-query`: DNS over HTTPS
///
/// `tls://` and `https://` accept `?ip=<addr>` to skip the startup hostname lookup.
///
/// `tcp://`, `tls://` and `https://` accept `?via=cpxy` to connect through the cpxy server, so
/// neither the query nor the answer can be tampered with on the way. The cpxy server then looks
/// up a hostname without `?ip=` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub endpoint: Endpoint,
    pub via: Via,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
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

/// The way connections to a DNS server go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Via {
    #[default]
    Direct,
    /// Through the cpxy server, which carries TCP only.
    Cpxy,
}

/// Where to connect to reach a DNS server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Addr(SocketAddr),
    /// A hostname for the cpxy server to look up.
    Name {
        host: String,
        port: u16,
    },
}

impl Display for Destination {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Addr(addr) => write!(f, "{addr}"),
            Self::Name { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

impl ServerSpec {
    pub fn direct(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            via: Via::Direct,
        }
    }

    /// The address to connect to. A hostname is looked up with the OS resolver, unless the
    /// connection goes through the cpxy server, which looks it up instead.
    pub async fn resolve(&self) -> anyhow::Result<Destination> {
        let (host, port, ip) = match &self.endpoint {
            Endpoint::Udp(addr) | Endpoint::Tcp(addr) => return Ok(Destination::Addr(*addr)),
            Endpoint::Tls { host, port, ip } | Endpoint::Https { host, port, ip, .. } => {
                (host, *port, ip)
            }
        };

        if let Some(ip) = ip {
            return Ok(Destination::Addr(SocketAddr::new(*ip, port)));
        }

        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(Destination::Addr(SocketAddr::new(ip, port)));
        }

        if self.via == Via::Cpxy {
            return Ok(Destination::Name {
                host: host.clone(),
                port,
            });
        }

        tokio::net::lookup_host((host.as_str(), port))
            .await
            .with_context(|| format!("Error looking up DNS server {host}"))?
            .next()
            .map(Destination::Addr)
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
            "tls" => Endpoint::Tls {
                host: parse_host(&url)?,
                port: url.port().unwrap_or(853),
                ip: parse_ip_param(&url)?,
            },
            "https" => Endpoint::Https {
                host: parse_host(&url)?,
                port: url.port_or_known_default().unwrap_or(443),
                path: match url.path() {
                    "" | "/" => DEFAULT_DOH_PATH.to_string(),
                    p => p.to_string(),
                },
                ip: parse_ip_param(&url)?,
            },
            scheme => bail!("Unsupported DNS server scheme {scheme}"),
        };
        let via = parse_via_param(&url)?;
        ensure!(
            via == Via::Direct || !matches!(endpoint, Endpoint::Udp(_)),
            "{s}: the cpxy server carries TCP only, use tcp://, tls:// or https://"
        );
        Ok(Self { endpoint, via })
    }
}

impl Display for ServerSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.endpoint {
            Endpoint::Udp(addr) => write!(f, "udp://{addr}")?,
            Endpoint::Tcp(addr) => write!(f, "tcp://{addr}")?,
            Endpoint::Tls { host, port, .. } => write!(f, "tls://{host}:{port}")?,
            Endpoint::Https {
                host, port, path, ..
            } => write!(f, "https://{host}:{port}{path}")?,
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
    fn parses_tls() {
        assert_eq!(
            "tls://dns.google".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Tls {
                host: "dns.google".to_string(),
                port: 853,
                ip: None,
            })
        );
        assert_eq!(
            "tls://dns.google:8853?ip=8.8.4.4"
                .parse::<ServerSpec>()
                .unwrap(),
            ServerSpec::direct(Endpoint::Tls {
                host: "dns.google".to_string(),
                port: 8853,
                ip: Some("8.8.4.4".parse().unwrap()),
            })
        );
    }

    #[test]
    fn parses_https() {
        assert_eq!(
            "https://dns.google".parse::<ServerSpec>().unwrap(),
            ServerSpec::direct(Endpoint::Https {
                host: "dns.google".to_string(),
                port: 443,
                path: "/dns-query".to_string(),
                ip: None,
            })
        );
        assert_eq!(
            "https://cloudflare-dns.com:8443/custom?ip=1.1.1.1"
                .parse::<ServerSpec>()
                .unwrap(),
            ServerSpec::direct(Endpoint::Https {
                host: "cloudflare-dns.com".to_string(),
                port: 8443,
                path: "/custom".to_string(),
                ip: Some("1.1.1.1".parse().unwrap()),
            })
        );
    }

    #[test]
    fn parses_via_cpxy() {
        assert_eq!(
            "tcp://1.1.1.1?via=cpxy".parse::<ServerSpec>().unwrap(),
            ServerSpec {
                endpoint: Endpoint::Tcp("1.1.1.1:53".parse().unwrap()),
                via: Via::Cpxy,
            }
        );
        assert_eq!(
            "https://dns.google/dns-query?ip=8.8.8.8&via=cpxy"
                .parse::<ServerSpec>()
                .unwrap(),
            ServerSpec {
                endpoint: Endpoint::Https {
                    host: "dns.google".to_string(),
                    port: 443,
                    path: "/dns-query".to_string(),
                    ip: Some("8.8.8.8".parse().unwrap()),
                },
                via: Via::Cpxy,
            }
        );
        let tls = "tls://dns.google?via=cpxy".parse::<ServerSpec>().unwrap();
        assert_eq!(tls.via, Via::Cpxy);
        assert_eq!(tls.to_string(), "tls://dns.google:853?via=cpxy");
    }

    #[tokio::test]
    async fn only_direct_hostnames_are_looked_up() {
        let through_cpxy = "https://dns.google?via=cpxy".parse::<ServerSpec>().unwrap();
        assert_eq!(
            through_cpxy.resolve().await.unwrap(),
            Destination::Name {
                host: "dns.google".to_string(),
                port: 443
            }
        );
        let with_ip = "tls://dns.google?ip=8.8.4.4&via=cpxy"
            .parse::<ServerSpec>()
            .unwrap();
        assert_eq!(
            with_ip.resolve().await.unwrap(),
            Destination::Addr("8.8.4.4:853".parse().unwrap())
        );
        let localhost = "tls://localhost:853".parse::<ServerSpec>().unwrap();
        assert!(matches!(
            localhost.resolve().await.unwrap(),
            Destination::Addr(addr) if addr.ip().is_loopback()
        ));
    }

    #[test]
    fn rejects_invalid_specs() {
        assert!("udp://dns.google".parse::<ServerSpec>().is_err());
        assert!("quic://dns.google".parse::<ServerSpec>().is_err());
        assert!("tls://dns.google?ip=nope".parse::<ServerSpec>().is_err());
        assert!("not a server".parse::<ServerSpec>().is_err());
        // The cpxy server carries TCP only
        assert!("udp://8.8.8.8?via=cpxy".parse::<ServerSpec>().is_err());
        assert!("tcp://8.8.8.8?via=socks".parse::<ServerSpec>().is_err());
    }
}
