use anyhow::{Context, ensure};
use client::dns_split::spec::ServerSpec;
use client::protocol_config::Config as ServerConfig;
use serde::Deserialize;
use std::net::Ipv4Addr;

/// The address the platform gives the TUN interface (a /30).
pub const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(10, 233, 0, 1);
pub const TUN_PREFIX_LEN: u8 = 30;
/// The DNS server the platform hands to the device. It only exists inside the engine.
pub const DNS_ADDR: Ipv4Addr = Ipv4Addr::new(10, 233, 0, 2);
/// Blocked names resolve here. The engine drops whatever is sent to [`is_sinkhole`] addresses
/// without a reply. OpenWrt uses the same range (`packaging/openwrt`), where it must not be
/// private: dnsmasq's rebind protection discards answers with private addresses.
pub const SINKHOLE_ADDR: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
pub const DEFAULT_MTU: u16 = 1500;

/// Whether `ip` is in 198.18.0.0/24, the range kept for blocked names: benchmarking space
/// (RFC 2544) that is never routed on the internet.
pub fn is_sinkhole(ip: Ipv4Addr) -> bool {
    matches!(ip.octets(), [198, 18, 0, _])
}

/// The engine configuration, as JSON from the app.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The cpxy server URL with its key, e.g. `https://:key@example.com`.
    pub server: String,
    /// IPv4 CIDRs forced through the proxy before automatic direct routing.
    #[serde(default)]
    pub proxy_routes: Vec<String>,
    /// DNS servers whose answer is used, keeping just its CN addresses, when it has any.
    pub dns_upstream: Vec<String>,
    /// DNS servers whose answer is used otherwise.
    pub dns_alternative: Vec<String>,
    /// Ordered suffixes for single-label DNS queries; empty leaves names unchanged.
    #[serde(default)]
    pub dns_search_domains: Vec<String>,
    #[serde(default)]
    pub mtu: Option<u16>,
    /// Resolves names on the built-in ad blocklist to [`SINKHOLE_ADDR`].
    #[serde(default)]
    pub block_ads: bool,
    /// Optional unauthenticated LAN proxy listeners, bound on all IPv4 interfaces.
    #[serde(default)]
    pub http_proxy_port: Option<u16>,
    #[serde(default)]
    pub socks_proxy_port: Option<u16>,
}

/// A config that has been checked and parsed.
pub struct Settings {
    pub server: ServerConfig,
    pub proxy_routes: client::outbound::ProxyRoutes,
    pub dns_upstream: Vec<ServerSpec>,
    pub dns_alternative: Vec<ServerSpec>,
    pub dns_search_domains: Vec<hickory_proto::rr::Name>,
    pub mtu: u16,
    pub block_ads: bool,
    pub http_proxy_port: Option<u16>,
    pub socks_proxy_port: Option<u16>,
}

impl Config {
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).context("Invalid engine config")
    }

    pub fn settings(&self) -> anyhow::Result<Settings> {
        let server = self.server.parse().context("Invalid server URL")?;
        let parse_specs = |name: &str, specs: &[String]| -> anyhow::Result<Vec<ServerSpec>> {
            ensure!(
                !specs.is_empty(),
                "At least one {name} DNS server is required"
            );
            specs.iter().map(|s| s.parse()).collect()
        };
        let mtu = self.mtu.unwrap_or(DEFAULT_MTU);
        ensure!((1280..=9000).contains(&mtu), "MTU {mtu} is out of range");

        ensure!(
            self.http_proxy_port != Some(0) && self.socks_proxy_port != Some(0),
            "Proxy ports must be between 1 and 65535"
        );
        ensure!(
            self.http_proxy_port.is_none() || self.http_proxy_port != self.socks_proxy_port,
            "HTTP and SOCKS proxy ports must be different"
        );

        Ok(Settings {
            server,
            proxy_routes: client::outbound::ProxyRoutes::parse(&self.proxy_routes)?,
            dns_upstream: parse_specs("upstream", &self.dns_upstream)?,
            dns_alternative: parse_specs("alternative", &self.dns_alternative)?,
            dns_search_domains: self
                .dns_search_domains
                .iter()
                .map(|value| client::dns_split::search::parse_search_domain(value))
                .collect::<anyhow::Result<_>>()?,
            mtu,
            block_ads: self.block_ads,
            http_proxy_port: self.http_proxy_port,
            socks_proxy_port: self.socks_proxy_port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::dns_split::spec::{Endpoint, Via};

    #[test]
    fn validates_search_domains_and_defaults_to_empty() {
        let base = r#"{"server":"http://:k@1.2.3.4", "dns_upstream":["1.1.1.1"], "dns_alternative":["8.8.8.8"]}"#;
        let mut config = Config::from_json(base).unwrap();
        assert!(config.settings().unwrap().dns_search_domains.is_empty());
        config.dns_search_domains = vec!["home.arpa".into(), "corp.example.".into()];
        assert_eq!(
            config
                .settings()
                .unwrap()
                .dns_search_domains
                .iter()
                .map(|n| n.to_ascii())
                .collect::<Vec<_>>(),
            ["home.arpa.", "corp.example."]
        );
        config.dns_search_domains = vec!["a..b".into()];
        assert!(config.settings().is_err());
    }

    #[test]
    fn parses_a_full_config() {
        let config = Config::from_json(
            r#"{
                "server": "https://:secret@proxy.example.com",
                "dns_upstream": ["223.5.5.5", "tcp://119.29.29.29"],
                "dns_alternative": ["https://dns.google/dns-query?ip=8.8.8.8"],
                "mtu": 1400
            }"#,
        )
        .unwrap();
        let settings = config.settings().unwrap();
        assert_eq!(settings.server.host, "proxy.example.com");
        assert_eq!(settings.server.port, 443);
        assert!(settings.server.tls);
        assert_eq!(settings.dns_upstream.len(), 2);
        assert!(matches!(
            settings.dns_alternative[0].endpoint,
            Endpoint::Https { .. }
        ));
        assert_eq!(settings.mtu, 1400);
    }

    #[test]
    fn validates_proxy_routes_and_defaults_to_empty() {
        let base = r#"{"server":"http://:k@1.2.3.4", "dns_upstream":["1.1.1.1"], "dns_alternative":["8.8.8.8"]}"#;
        let mut config = Config::from_json(base).unwrap();
        assert_eq!(
            config.settings().unwrap().proxy_routes,
            client::outbound::ProxyRoutes::default()
        );
        config.proxy_routes = vec!["223.5.5.5/32".into()];
        assert!(
            config
                .settings()
                .unwrap()
                .proxy_routes
                .contains(Some("223.5.5.5".parse().unwrap()))
        );
        config.proxy_routes = vec!["::/0".into()];
        assert!(config.settings().is_err());
        config.proxy_routes = vec!["10.0.0.0/33".into()];
        assert!(config.settings().is_err());
    }

    #[test]
    fn defaults() {
        let config = Config::from_json(
            r#"{"server": "http://:k@1.2.3.4:80", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"]}"#,
        )
        .unwrap();
        let settings = config.settings().unwrap();
        assert_eq!(settings.mtu, 1500);
        assert!(!settings.block_ads);
        assert_eq!(settings.http_proxy_port, None);
        assert_eq!(settings.socks_proxy_port, None);
    }

    #[test]
    fn proxy_ports_are_optional_and_validated() {
        let check = |http: &str, socks: &str| {
            Config::from_json(&format!(r#"{{"server":"http://:k@1.2.3.4", "dns_upstream":["1.1.1.1"], "dns_alternative":["8.8.8.8"], "http_proxy_port":{http}, "socks_proxy_port":{socks}}}"#)).and_then(|c| c.settings())
        };
        let settings = check("8080", "1080").unwrap();
        assert_eq!(settings.http_proxy_port, Some(8080));
        assert_eq!(settings.socks_proxy_port, Some(1080));
        assert!(check("1", "65535").is_ok());
        assert!(check("null", "1080").is_ok());
        for (http, socks) in [
            ("0", "null"),
            ("null", "0"),
            ("8080", "8080"),
            ("65536", "null"),
            ("-1", "null"),
        ] {
            assert!(check(http, socks).is_err());
        }
    }

    #[test]
    fn block_ads_is_read() {
        let config = Config::from_json(
            r#"{"server": "http://:k@1.2.3.4:80", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"], "block_ads": true}"#,
        )
        .unwrap();
        assert!(config.settings().unwrap().block_ads);
    }

    #[test]
    fn reads_servers_via_cpxy() {
        let config = Config::from_json(
            r#"{"server": "http://:k@1.2.3.4:80", "dns_upstream": ["223.5.5.5"], "dns_alternative": ["tcp://1.1.1.1?via=cpxy", "8.8.8.8"]}"#,
        )
        .unwrap();
        let settings = config.settings().unwrap();
        assert_eq!(settings.dns_alternative[0].via, Via::Cpxy);
        assert_eq!(settings.dns_alternative[1].via, Via::Direct);
    }

    #[test]
    fn sinkhole_range() {
        assert!(is_sinkhole(SINKHOLE_ADDR));
        assert!(is_sinkhole(Ipv4Addr::new(198, 18, 0, 255)));
        assert!(!is_sinkhole(DNS_ADDR));
        assert!(!is_sinkhole(Ipv4Addr::new(198, 18, 1, 1)));
    }

    #[test]
    fn rejects_bad_configs() {
        let settings = |json: &str| Config::from_json(json).and_then(|c| c.settings());
        // No key in the server URL
        assert!(
            settings(r#"{"server": "http://1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"]}"#)
                .is_err()
        );
        // No alternative DNS server
        assert!(
            settings(r#"{"server": "http://:k@1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": []}"#)
                .is_err()
        );
        // Unknown field, e.g. a typo
        assert!(
            settings(r#"{"server": "http://:k@1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"], "mut": 1}"#)
                .is_err()
        );
        // MTU below the IPv6 minimum
        assert!(
            settings(r#"{"server": "http://:k@1.2.3.4", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"], "mtu": 576}"#)
                .is_err()
        );
    }
}
