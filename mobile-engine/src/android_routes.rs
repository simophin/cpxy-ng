//! Android route plans use the same reviewed dataset and forced cuts as userspace routing.
use client::{ipv6::Ipv6Mode, outbound::ProxyRoutes};
use std::net::{Ipv4Addr, Ipv6Addr};

/// A bounded plan suitable for one Android VPN establishment. Exclusions require API 33.
#[derive(Debug, uniffi::Record)]
pub struct AndroidRoutePlan {
    pub included: Vec<String>,
    pub excluded: Vec<String>,
    /// Empty for exact coverage; otherwise captured CN/private traffic is relayed in userspace.
    pub fallback_reason: String,
}

const ROUTE_BUDGET: usize = 512;

pub fn plan(routes: &ProxyRoutes, ipv6: Ipv6Mode, exclusions: bool) -> AndroidRoutePlan {
    let mut v4 = vec![(u32::from(crate::DNS_ADDR), u32::from(crate::DNS_ADDR))];
    let mut v6 = Vec::new();
    for net in &routes.0 {
        match net {
            ipnet::IpNet::V4(net) => v4.push((net.network().into(), net.broadcast().into())),
            ipnet::IpNet::V6(net) => v6.push((net.network().into(), net.broadcast().into())),
        }
    }
    let direct4 = geoip_data::routes::direct_ipv4(&v4);
    let direct6 = if ipv6 == Ipv6Mode::Off {
        vec![]
    } else {
        geoip_data::routes::router_direct_ipv6(&v6)
    };
    // Bound each family independently so a fragmented IPv6 dataset cannot remove IPv4
    // native bypass. Dropping narrow direct prefixes only captures more traffic.
    let budget = ROUTE_BUDGET / 2;
    let mut max4 = 32;
    let routes4 = loop {
        let direct: Vec<_> = direct4
            .iter()
            .copied()
            .filter(|&(ip, p)| p <= max4 || client::outbound::is_private(Ipv4Addr::from(ip).into()))
            .collect();
        let list = if exclusions {
            direct
        } else {
            geoip_data::routes::complement_ipv4(&direct)
        };
        if list.len() < budget {
            break list;
        }
        if max4 == 0 {
            break if exclusions { vec![] } else { vec![(0, 0)] };
        }
        max4 -= 1;
    };
    let mut max6 = 128;
    let routes6 = loop {
        let direct: Vec<_> = direct6
            .iter()
            .copied()
            .filter(|&(ip, p)| p <= max6 || client::outbound::is_private(Ipv6Addr::from(ip).into()))
            .collect();
        let list = if exclusions {
            direct
        } else {
            geoip_data::routes::complement_ipv6(&direct)
        };
        if list.len() < budget {
            break list;
        }
        if max6 == 0 {
            break if exclusions { vec![] } else { vec![(0, 0)] };
        }
        max6 -= 1;
    };
    let prefixes: Vec<_> = routes4
        .into_iter()
        .map(|(ip, p)| format!("{}/{p}", Ipv4Addr::from(ip)))
        .chain(
            routes6
                .into_iter()
                .map(|(ip, p)| format!("{}/{p}", Ipv6Addr::from(ip))),
        )
        .collect();
    AndroidRoutePlan {
        included: if exclusions {
            vec!["0.0.0.0/0".into(), "::/0".into()]
        } else {
            prefixes.clone()
        },
        excluded: if exclusions { prefixes } else { vec![] },
        fallback_reason: if max4 == 32 && max6 == 128 {
            String::new()
        } else {
            format!(
                "Route budget {ROUTE_BUDGET}: exact direct prefixes v4={}, v6={}; retaining prefixes <= /{max4}, /{max6}; captured CN/private flows use direct sockets",
                direct4.len(),
                direct6.len()
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{Route, route};
    use std::net::IpAddr;

    fn captured(plan: &AndroidRoutePlan, ip: IpAddr) -> bool {
        let has = |list: &[String]| {
            list.iter()
                .any(|s| s.parse::<ipnet::IpNet>().unwrap().contains(&ip))
        };
        has(&plan.included) && !has(&plan.excluded)
    }

    #[test]
    fn both_platforms_never_bypass_proxy_or_virtual_dns() {
        let routes = ProxyRoutes::parse(&[
            "223.5.5.5/32".into(),
            "2400:3200::/32".into(),
            "10.0.0.0/9".into(),
        ])
        .unwrap();
        for mode in [Ipv6Mode::Full, Ipv6Mode::Direct, Ipv6Mode::Off] {
            for exclusions in [false, true] {
                let plan = plan(&routes, mode, exclusions);
                eprintln!(
                    "{mode:?} exclusions={exclusions}: included={} excluded={} {}",
                    plan.included.len(),
                    plan.excluded.len(),
                    plan.fallback_reason
                );
                assert!(plan.included.len() + plan.excluded.len() <= ROUTE_BUDGET);
                for ip in [
                    "10.233.0.2",
                    "223.5.5.5",
                    "2400:3200::1",
                    "8.8.8.8",
                    "::",
                    "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
                ] {
                    assert!(captured(&plan, ip.parse().unwrap()), "{ip}");
                }
                let mut state = 0x123456789abcdef0u128;
                for _ in 0..2000 {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    for ip in [IpAddr::V4((state as u32).into()), IpAddr::V6(state.into())] {
                        if route(&routes, Some(ip)) == Route::Proxy
                            || (mode == Ipv6Mode::Off && ip.is_ipv6())
                        {
                            assert!(captured(&plan, ip), "foreign bypass: {ip}");
                        }
                    }
                }
                assert!(!captured(&plan, "192.168.1.1".parse().unwrap()));
            }
        }
    }

    #[test]
    fn broad_forced_routes_capture_everything() {
        let routes = ProxyRoutes::parse(&["0.0.0.0/0".into(), "::/0".into()]).unwrap();
        for exclusions in [false, true] {
            let plan = plan(&routes, Ipv6Mode::Full, exclusions);
            assert!(plan.excluded.is_empty());
            assert!(plan.fallback_reason.is_empty());
            assert_eq!(plan.included, ["0.0.0.0/0", "::/0"]);
        }
    }
}
