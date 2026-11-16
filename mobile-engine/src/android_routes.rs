//! Android route plans use the same reviewed dataset and forced cuts as userspace routing.
//!
//! Only IPv4 bypasses the VPN. All IPv6 is captured and refused by the engine, and the TUN has
//! no IPv6 address, so apps use IPv4. The app's own sockets (the proxy transport) are excluded
//! from the VPN by UID and keep using the underlying network natively, IPv6 included.
use client::outbound::ProxyRoutes;
use client::region::LocalRegion;
use std::net::Ipv4Addr;

/// A bounded plan suitable for one Android VPN establishment. Exclusions require API 33.
#[derive(Debug, uniffi::Record)]
pub struct AndroidRoutePlan {
    pub included: Vec<String>,
    pub excluded: Vec<String>,
    /// Empty for exact coverage; otherwise captured CN/private traffic is relayed in userspace.
    pub fallback_reason: String,
}

/// The routes for a device in `region`: its local region's and private IPv4 bypass the VPN,
/// except `routes`; everything else, and all IPv6, goes into it.
pub fn plan(
    routes: &ProxyRoutes,
    region: LocalRegion,
    exclusions: bool,
    route_budget: usize,
) -> AndroidRoutePlan {
    let mut v4 = vec![(u32::from(crate::DNS_ADDR), u32::from(crate::DNS_ADDR))];
    for net in &routes.0 {
        if let ipnet::IpNet::V4(net) = net {
            v4.push((net.network().into(), net.broadcast().into()));
        }
    }
    let direct4 = match region {
        LocalRegion::Cn => geoip_data::routes::direct_ipv4(&v4),
        LocalRegion::Overseas => geoip_data::routes::overseas_direct_ipv4(&v4),
    };
    // One route is reserved for capturing all IPv6.
    let budget = route_budget - 1;
    let mut max4 = 32;
    let routes4 = loop {
        let direct: Vec<_> = direct4
            .iter()
            .copied()
            .filter(|&(ip, p)| p <= max4 || client::outbound::is_private(Ipv4Addr::from(ip).into()))
            .collect();
        // Capturing everything and excluding the direct set, or including only the captured set,
        // whichever is shorter: outside China the captured set is much the smaller.
        let captured = geoip_data::routes::complement_ipv4(&direct);
        let exclude = exclusions && direct.len() <= captured.len();
        let list = if exclude { direct } else { captured };
        if list.len() < budget {
            break (exclude, list);
        }
        if max4 == 0 {
            break if exclusions {
                (true, vec![])
            } else {
                (false, vec![(0, 0)])
            };
        }
        max4 -= 1;
    };
    let (exclusions, routes4) = routes4;
    let prefixes: Vec<String> = routes4
        .into_iter()
        .map(|(ip, p)| format!("{}/{p}", Ipv4Addr::from(ip)))
        .collect();
    let (included, excluded) = if exclusions {
        (vec!["0.0.0.0/0".into(), "::/0".into()], prefixes)
    } else {
        let mut included = prefixes;
        included.push("::/0".into());
        (included, vec![])
    };
    AndroidRoutePlan {
        included,
        excluded,
        fallback_reason: if max4 == 32 {
            String::new()
        } else {
            format!(
                "Route budget {route_budget}: exact direct prefixes v4={}; retaining prefixes <= /{max4}; captured local-region/private flows use direct sockets",
                direct4.len(),
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{Policy, Route};
    use std::net::IpAddr;

    fn captured(plan: &AndroidRoutePlan, ip: IpAddr) -> bool {
        let has = |list: &[String]| {
            list.iter()
                .any(|s| s.parse::<ipnet::IpNet>().unwrap().contains(&ip))
        };
        has(&plan.included) && !has(&plan.excluded)
    }

    #[test]
    fn exact_plan_is_available_before_platform_fallback() {
        for exclusions in [false, true] {
            let plan = plan(
                &ProxyRoutes::default(),
                LocalRegion::Cn,
                exclusions,
                100_000,
            );
            assert!(plan.fallback_reason.is_empty());
            assert!(!captured(&plan, "223.5.5.5".parse().unwrap()));
            // CN IPv6 is captured too: the engine refuses all IPv6.
            assert!(captured(&plan, "2400:3200::1".parse().unwrap()));
            assert!(captured(&plan, "10.233.0.2".parse().unwrap()));
            eprintln!(
                "Exact exclusions={exclusions}: included={} excluded={}",
                plan.included.len(),
                plan.excluded.len()
            );
        }
    }

    #[test]
    fn both_platforms_never_bypass_proxy_or_virtual_dns() {
        let routes = ProxyRoutes::parse(&[
            "223.5.5.5/32".into(),
            "2400:3200::/32".into(),
            "10.0.0.0/9".into(),
        ])
        .unwrap();
        let policy = Policy {
            region: LocalRegion::Cn,
            proxy_routes: routes.clone(),
        };
        for exclusions in [false, true] {
            let plan = plan(&routes, LocalRegion::Cn, exclusions, 512);
            eprintln!(
                "exclusions={exclusions}: included={} excluded={} {}",
                plan.included.len(),
                plan.excluded.len(),
                plan.fallback_reason
            );
            assert!(plan.included.len() + plan.excluded.len() <= 512);
            for ip in [
                "10.233.0.2",
                "223.5.5.5",
                "2400:3200::1",
                "8.8.8.8",
                "::",
                "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
                "fe80::1",
                "fc00::1",
            ] {
                assert!(captured(&plan, ip.parse().unwrap()), "{ip}");
            }
            let mut state = 0x123456789abcdef0u128;
            for _ in 0..2000 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                for ip in [IpAddr::V4((state as u32).into()), IpAddr::V6(state.into())] {
                    if policy.route(Some(ip)) == Route::Proxy || ip.is_ipv6() {
                        assert!(captured(&plan, ip), "foreign bypass: {ip}");
                    }
                }
            }
            assert!(!captured(&plan, "192.168.1.1".parse().unwrap()));
        }
    }

    #[test]
    fn broad_forced_routes_capture_everything() {
        let routes = ProxyRoutes::parse(&["0.0.0.0/0".into(), "::/0".into()]).unwrap();
        for exclusions in [false, true] {
            let plan = plan(&routes, LocalRegion::Cn, exclusions, 512);
            assert!(plan.excluded.is_empty());
            assert!(plan.fallback_reason.is_empty());
            assert_eq!(plan.included, ["0.0.0.0/0", "::/0"]);
        }
    }

    #[test]
    fn overseas_plan_captures_cn_and_lets_the_rest_bypass() {
        for exclusions in [false, true] {
            let plan = plan(
                &ProxyRoutes::default(),
                LocalRegion::Overseas,
                exclusions,
                100_000,
            );
            assert!(plan.fallback_reason.is_empty(), "{}", plan.fallback_reason);
            for ip in [
                "223.5.5.5",
                "223.6.6.6",
                "10.233.0.2",
                "2400:3200::1",
                "2001:4860::1",
            ] {
                assert!(
                    captured(&plan, ip.parse().unwrap()),
                    "{ip} must be captured"
                );
            }
            for ip in ["1.1.1.1", "8.8.8.8", "192.168.1.1"] {
                assert!(!captured(&plan, ip.parse().unwrap()), "{ip} must bypass");
            }
        }
    }

    /// Android refuses a VPN with too many routes in one binder transaction: the exact overseas
    /// exclusion list (~18k) was refused on an emulator, the in-China one (~8k) was not.
    #[test]
    fn plans_use_the_shorter_of_inclusions_and_exclusions() {
        let size = |region| {
            let plan = plan(&ProxyRoutes::default(), region, true, 100_000);
            assert!(plan.fallback_reason.is_empty());
            plan.included.len() + plan.excluded.len()
        };
        let cn = size(LocalRegion::Cn);
        let overseas = size(LocalRegion::Overseas);
        assert!(overseas <= cn + 2, "overseas {overseas}, in China {cn}");
    }

    #[test]
    fn overseas_plan_never_bypasses_proxied_destinations() {
        let routes = ProxyRoutes::parse(&["8.8.4.0/24".into()]).unwrap();
        let policy = Policy {
            region: LocalRegion::Overseas,
            proxy_routes: routes.clone(),
        };
        for exclusions in [false, true] {
            let plan = plan(&routes, LocalRegion::Overseas, exclusions, 512);
            assert!(plan.included.len() + plan.excluded.len() <= 512);
            assert!(captured(&plan, "8.8.4.4".parse().unwrap()));
            let mut state = 0x0fedcba987654321u128;
            for _ in 0..2000 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                for ip in [IpAddr::V4((state as u32).into()), IpAddr::V6(state.into())] {
                    if policy.route(Some(ip)) == Route::Proxy || ip.is_ipv6() {
                        assert!(captured(&plan, ip), "proxied destination bypasses: {ip}");
                    }
                }
            }
        }
    }
}
