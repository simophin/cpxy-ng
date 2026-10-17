/// The IPv4 ranges of the local region, serialized for `cpxy_ng::geoip::contains_v4`.
pub static CN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cn.dat"));
