//! Which side of the CN GeoIP boundary is local: its addresses connect directly, the rest go
//! through the cpxy server, and its resolvers' answers win the split DNS race.

use crate::outbound::is_private;
use cpxy_ng::geoip::{contains_v4, contains_v6};
use geoip_data::{CN, CN6};
use std::net::IpAddr;

/// Where the device is, on the CN GeoIP boundary. Spelled `cn` and `overseas` on the command line
/// and in JSON.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum LocalRegion {
    /// In China: CN addresses are local, the rest are remote
    #[default]
    Cn,
    /// Outside China ("reverse" mode): CN addresses are remote, the rest are local
    Overseas,
}

impl LocalRegion {
    /// Whether the address is in this region. Private, loopback and link-local addresses are in
    /// neither: callers decide on them by themselves.
    pub fn contains(self, ip: IpAddr) -> bool {
        self.predicate()(ip)
    }

    /// [`Self::contains`] as a function, for code that keeps a plain `fn`.
    pub fn predicate(self) -> fn(IpAddr) -> bool {
        match self {
            LocalRegion::Cn => is_cn_ip,
            LocalRegion::Overseas => is_overseas_ip,
        }
    }
}

/// Whether the embedded GeoIP dataset places the address in China.
pub fn is_cn_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => contains_v4(&ip, CN),
        IpAddr::V6(ip) => contains_v6(&ip, CN6),
    }
}

/// A public address outside China.
fn is_overseas_ip(ip: IpAddr) -> bool {
    !is_private(ip) && !is_cn_ip(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_addresses_of_both_families() {
        // AliDNS and DNSPod, over IPv4 and IPv6
        let cn = ["223.5.5.5", "2400:3200::1", "2402:4e00::"];
        let overseas = [
            "8.8.8.8",
            "1.1.1.1",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
        ];
        for ip in cn {
            let ip = ip.parse().unwrap();
            assert!(LocalRegion::Cn.contains(ip), "{ip}");
            assert!(!LocalRegion::Overseas.contains(ip), "{ip}");
        }
        for ip in overseas {
            let ip = ip.parse().unwrap();
            assert!(!LocalRegion::Cn.contains(ip), "{ip}");
            assert!(LocalRegion::Overseas.contains(ip), "{ip}");
        }
    }

    #[test]
    fn private_addresses_are_in_neither_region() {
        for ip in ["10.0.0.1", "192.168.1.1", "127.0.0.1", "fd00::1", "fe80::1"] {
            let ip = ip.parse().unwrap();
            assert!(!LocalRegion::Cn.contains(ip), "{ip}");
            assert!(!LocalRegion::Overseas.contains(ip), "{ip}");
        }
    }
}
