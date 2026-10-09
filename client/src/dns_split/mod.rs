//! A forwarding DNS server that races queries across two groups of servers and picks an
//! answer based on whether its addresses are in China according to the embedded GeoIP dataset.

pub mod cache;
pub mod policy;
pub mod server;
pub mod spec;
pub mod upstream;

use cpxy_ng::geoip::find_country_code_v4;
use geoip_data::GEOIP;
use std::net::Ipv4Addr;

/// Whether the embedded GeoIP dataset places the address in China.
pub fn is_local_region_ip(ip: Ipv4Addr) -> bool {
    matches!(find_country_code_v4(&ip, GEOIP), Ok(Some("CN")))
}
