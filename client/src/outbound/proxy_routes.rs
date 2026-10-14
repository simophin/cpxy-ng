use ipnet::Ipv4Net;
use std::net::Ipv4Addr;
use std::str::FromStr;

/// Explicit IPv4 networks that must use the main proxy, before automatic routing rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyRoutes(pub Vec<Ipv4Net>);

impl ProxyRoutes {
    pub fn contains(&self, ip: Option<Ipv4Addr>) -> bool {
        ip.is_some_and(|ip| self.0.iter().any(|net| net.contains(&ip)))
    }

    pub fn parse(routes: &[String]) -> anyhow::Result<Self> {
        routes
            .iter()
            .map(|route| Self::parse_network(route))
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Self)
    }

    pub fn parse_network(route: &str) -> anyhow::Result<Ipv4Net> {
        Ipv4Net::from_str(route.trim())
            .map_err(|_| anyhow::anyhow!("Invalid IPv4 proxy route CIDR: {route}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_network_boundaries_and_host_routes() {
        let routes = ProxyRoutes::parse(&["10.2.3.99/24".into(), "223.5.5.5/32".into()]).unwrap();
        for ip in ["10.2.3.0", "10.2.3.255", "223.5.5.5"] {
            assert!(routes.contains(Some(ip.parse().unwrap())));
        }
        for ip in ["10.2.2.255", "10.2.4.0", "223.5.5.6"] {
            assert!(!routes.contains(Some(ip.parse().unwrap())));
        }
        assert!(!routes.contains(None));
        assert!(!ProxyRoutes::default().contains(Some("10.2.3.4".parse().unwrap())));
        assert!(
            ProxyRoutes::parse(&["0.0.0.0/0".into()])
                .unwrap()
                .contains(Some("255.255.255.255".parse().unwrap()))
        );
    }

    #[test]
    fn rejects_invalid_or_unsupported_routes() {
        for route in ["example.com/24", "10.0.0.1", "10.0.0.0/33", "::/0", ""] {
            assert!(ProxyRoutes::parse(&[route.into()]).is_err(), "{route}");
        }
    }
}
