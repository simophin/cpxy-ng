/// IPv4 ranges of every country, serialized for `cpxy_ng::geoip::find_country_code_v4`.
pub static GEOIP: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/geoip.dat"));
