//! The connection profile the app hands to the engine as JSON. The key is passed separately and
//! never appears here.

use anyhow::{Context, ensure};
use client::dns_split::spec::{ServerSpec, Via};
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// `host:port` of the RTP endpoint's HTTP control port; the host may be a name or an IP literal.
    pub endpoint: String,
    /// Local-region resolvers, asked directly (`client::dns_split::spec` syntax).
    pub dns_upstream: Vec<String>,
    /// Resolvers asked over UDP through the tunnel.
    pub dns_alternative: Vec<Ipv4Addr>,
    #[serde(default)]
    pub dns_local_server: Option<String>,
    #[serde(default)]
    pub dns_local_domains: Vec<String>,
    #[serde(default)]
    pub dns_search_domains: Vec<String>,
}

/// A checked [`Profile`].
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub dns_upstream: Vec<ServerSpec>,
    pub dns_alternative: Vec<Ipv4Addr>,
    pub dns_local_server: Option<ServerSpec>,
    pub dns_local_domains: Vec<hickory_proto::rr::Name>,
    pub dns_search_domains: Vec<hickory_proto::rr::Name>,
}

impl Profile {
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).context("Invalid profile")
    }

    pub fn settings(&self) -> anyhow::Result<Settings> {
        let (host, port) = split_host_port(&self.endpoint)?;
        ensure!(
            !self.dns_upstream.is_empty(),
            "At least one upstream DNS server is required"
        );
        ensure!(
            !self.dns_alternative.is_empty(),
            "At least one alternative DNS server is required"
        );
        let dns_upstream = self
            .dns_upstream
            .iter()
            .map(|s| {
                let spec: ServerSpec = s
                    .parse()
                    .with_context(|| format!("Invalid upstream DNS server {s}"))?;
                ensure!(
                    spec.via == Via::Direct,
                    "Upstream DNS server {s} cannot use via=cpxy"
                );
                Ok(spec)
            })
            .collect::<anyhow::Result<_>>()?;
        for &ip in &self.dns_alternative {
            ensure!(
                !(ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.is_multicast()
                    || ip.is_broadcast()
                    || ip == crate::DNS_ADDRESS),
                "Invalid alternative DNS server {ip}"
            );
        }
        let dns_local_server = self
            .dns_local_server
            .as_ref()
            .map(|text| text.parse::<ServerSpec>())
            .transpose()?;
        if let Some(server) = &dns_local_server {
            ensure!(
                server.via == Via::Direct,
                "Local DNS must use host networking"
            );
        }
        let names = |values: &Vec<String>| {
            values
                .iter()
                .map(|name| hickory_proto::rr::Name::from_ascii(name).map_err(anyhow::Error::from))
                .collect::<anyhow::Result<Vec<_>>>()
        };
        Ok(Settings {
            host,
            port,
            dns_upstream,
            dns_alternative: self.dns_alternative.clone(),
            dns_local_server,
            dns_local_domains: names(&self.dns_local_domains)?,
            dns_search_domains: names(&self.dns_search_domains)?,
        })
    }
}

impl Settings {
    /// Looks the endpoint up with the system resolver, which the engine's sockets use outside the
    /// tunnel. Prefers IPv4, which every network can reach through NAT64 or directly.
    pub fn resolve_endpoint(&self) -> anyhow::Result<SocketAddr> {
        let addrs: Vec<_> = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .with_context(|| format!("Cannot resolve {}", self.host))?
            .collect();
        addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or(addrs.first())
            .copied()
            .with_context(|| format!("No address for {}", self.host))
    }
}

/// `host:port`, `[v6]:port` or `v4:port`.
fn split_host_port(endpoint: &str) -> anyhow::Result<(String, u16)> {
    if let Ok(addr) = endpoint.parse::<SocketAddr>() {
        ensure!(addr.port() != 0, "Endpoint port must not be 0");
        return Ok((addr.ip().to_string(), addr.port()));
    }
    let (host, port) = endpoint
        .rsplit_once(':')
        .with_context(|| format!("Endpoint {endpoint} needs a port"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("Invalid endpoint port in {endpoint}"))?;
    ensure!(port != 0, "Endpoint port must not be 0");
    ensure!(
        !host.is_empty()
            && !host.contains(':')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'),
        "Invalid endpoint host {host}"
    );
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile::from_json(
            r#"{"endpoint":"vpn.example.net:30000",
            "dns_upstream":["223.5.5.5","tls://dns.alidns.com"],"dns_alternative":["8.8.8.8"]}"#,
        )
        .unwrap()
    }

    #[test]
    fn accepts_names_and_literals() {
        let s = profile().settings().unwrap();
        assert_eq!((s.host.as_str(), s.port), ("vpn.example.net", 30000));
        assert_eq!(s.dns_upstream.len(), 2);
        for (endpoint, host) in [
            ("192.0.2.1:1", "192.0.2.1"),
            ("[2001:db8::1]:2", "2001:db8::1"),
        ] {
            let mut p = profile();
            p.endpoint = endpoint.into();
            assert_eq!(p.settings().unwrap().host, host);
        }
    }

    #[test]
    fn rejects_invalid_profiles() {
        let bad: [fn(&mut Profile); 10] = [
            |p| p.endpoint = "vpn.example.net".into(),
            |p| p.endpoint = "vpn.example.net:0".into(),
            |p| p.endpoint = ":30000".into(),
            |p| p.endpoint = "a b:30000".into(),
            |p| p.dns_local_server = Some("tcp://1.1.1.1?via=cpxy".into()),
            |p| p.dns_local_domains = vec!["bad domain".into()],
            |p| p.dns_upstream.clear(),
            |p| p.dns_upstream = vec!["tcp://1.1.1.1?via=cpxy".into()],
            |p| p.dns_alternative.clear(),
            |p| p.dns_alternative = vec![Ipv4Addr::LOCALHOST],
        ];
        for (i, change) in bad.iter().enumerate() {
            let mut p = profile();
            change(&mut p);
            assert!(p.settings().is_err(), "case {i}");
        }
        assert!(Profile::from_json(r#"{"endpoint":"a:1","address":"10.0.0.2","dns_upstream":[],"dns_alternative":[],"key":"x"}"#).is_err());
    }

    #[test]
    fn resolves_literal_endpoints() {
        let mut p = profile();
        p.endpoint = "192.0.2.1:30000".into();
        assert_eq!(
            p.settings().unwrap().resolve_endpoint().unwrap(),
            "192.0.2.1:30000".parse::<SocketAddr>().unwrap()
        );
    }
}
