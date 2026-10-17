pub mod cn;
mod direct;
mod either;
mod http;
mod ip_divert;
mod protocol;
mod proxy_routes;
mod resolving_ip;
mod site_divert;
mod stat_reporting;

pub use direct::*;
pub use either::*;
pub use http::*;
pub use ip_divert::*;
pub use protocol::*;
pub use proxy_routes::*;
pub use resolving_ip::*;
pub use site_divert::*;
pub use stat_reporting::*;

use std::net::IpAddr;

/// Private, loopback and link-local addresses, which only the local network reaches.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local(),
    }
}
