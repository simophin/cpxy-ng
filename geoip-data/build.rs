use cpxy_ng::geoip::{GeoIPv4Entry, serialize_entries};
use geoip_v2ray::proto::GeoIp;
use ipnet::Ipv4Net;
use prost::Message;
use reqwest::blocking::get;
use std::fs::File;
use std::io::Read;
use std::net::Ipv4Addr;
use std::path::Path;

fn main() {
    let output_file = Path::new(std::env::var("OUT_DIR").unwrap().as_str()).join("geoip.dat");
    if output_file.exists() {
        return;
    }

    let mut resp = get("https://cdn.jsdelivr.net/gh/Loyalsoldier/geoip@release/geoip.dat")
        .expect("Could not get response");

    let mut buf = Default::default();
    resp.read_to_end(&mut buf).expect("To read all files");

    let list = geoip_v2ray::proto::GeoIpList::decode(buf.as_slice()).expect("To deserialize data");
    let entries: Vec<_> = list
        .entry
        .into_iter()
        .filter(|i| {
            i.country_code.eq_ignore_ascii_case("china")
                || i.country_code.eq_ignore_ascii_case("cn")
        })
        .flat_map(|GeoIp { cidr, .. }| {
            let country_code = *b"CN";

            cidr.into_iter()
                .filter_map(|cidr| {
                    if cidr.ip.len() == 4 {
                        Some((
                            Ipv4Addr::from([cidr.ip[0], cidr.ip[1], cidr.ip[2], cidr.ip[3]]),
                            cidr.prefix,
                        ))
                    } else {
                        None
                    }
                })
                .map(move |(addr, prefix)| {
                    let net = Ipv4Net::new(addr, prefix as u8).unwrap();
                    GeoIPv4Entry::new(addr, net.broadcast(), country_code)
                })
        })
        .collect();

    let mut file = File::options()
        .write(true)
        .open(output_file)
        .expect("Could not create GeoIP archive");
    serialize_entries(&mut file, entries).expect("Could not serialize GeoIP archive");
}
