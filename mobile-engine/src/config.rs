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
pub const DEFAULT_MTU: u16 = 1500;

/// The engine configuration, as JSON from the app.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The cpxy server URL with its key, e.g. `https://:key@example.com`.
    pub server: String,
    /// DNS servers whose answer is used when all its addresses are in CN.
    pub dns_upstream: Vec<String>,
    /// DNS servers whose answer is used otherwise.
    pub dns_alternative: Vec<String>,
    #[serde(default)]
    pub mtu: Option<u16>,
}

/// A config that has been checked and parsed.
pub struct Settings {
    pub server: ServerConfig,
    pub dns_upstream: Vec<ServerSpec>,
    pub dns_alternative: Vec<ServerSpec>,
    pub mtu: u16,
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

        Ok(Settings {
            server,
            dns_upstream: parse_specs("upstream", &self.dns_upstream)?,
            dns_alternative: parse_specs("alternative", &self.dns_alternative)?,
            mtu,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            settings.dns_alternative[0],
            ServerSpec::Https { .. }
        ));
        assert_eq!(settings.mtu, 1400);
    }

    #[test]
    fn mtu_defaults_to_1500() {
        let config = Config::from_json(
            r#"{"server": "http://:k@1.2.3.4:80", "dns_upstream": ["1.1.1.1"], "dns_alternative": ["8.8.8.8"]}"#,
        )
        .unwrap();
        assert_eq!(config.settings().unwrap().mtu, 1500);
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
