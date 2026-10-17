//! A forwarding DNS server that races queries across two groups of servers and picks an
//! answer based on whether its addresses are in China according to the embedded GeoIP dataset.

pub mod cache;
pub mod policy;
pub mod search;
pub mod server;
pub mod spec;
pub mod upstream;

use cpxy_ng::geoip::{contains_v4, contains_v6};
use geoip_data::{CN, CN6};
use std::net::IpAddr;

/// Whether the embedded GeoIP dataset places the address in China.
pub fn is_local_region_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => contains_v4(&ip, CN),
        IpAddr::V6(ip) => contains_v6(&ip, CN6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_addresses_of_both_families() {
        // AliDNS and DNSPod, over IPv4 and IPv6
        for ip in ["223.5.5.5", "2400:3200::1", "2402:4e00::"] {
            assert!(is_local_region_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
        ] {
            assert!(!is_local_region_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}
