/// The IPv4 ranges of the local region, serialized for `cpxy_ng::geoip::contains_v4`.
pub static CN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cn.dat"));

/// The IPv6 ranges of the local region, serialized for `cpxy_ng::geoip::contains_v6`.
pub static CN6: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cn6.dat"));

pub mod routes;

/// Identity of the reviewed IPv4 data used by DNS, userspace policy and route export.
pub const IPV4_SHA256: &str = env!("CPXY_GEOIP_V4_SHA256");
