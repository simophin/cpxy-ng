//! Native OpenWrt TUN client; routing and device persistence belong to the init script.
#[cfg(any(target_os = "linux", test))]
mod cli {
    use clap::Parser;

    #[derive(Parser)]
    pub struct Options {
        /// Required plaintext, no-auth SOCKS5 endpoint. May be cpxy-client on loopback.
        #[arg(long, env = "SOCKS5_PROXY", required_unless_present_any = ["export_direct_routes", "export_direct_routes_v6", "geoip_version"])]
        pub socks5_proxy: Option<std::net::SocketAddr>,
        /// Print exact IPv6 CN/private direct CIDRs, excluding --proxy-route, then exit.
        #[arg(long)]
        pub export_direct_routes_v6: bool,
        /// Print exact IPv4 direct CIDRs, excluding --proxy-route, then exit without a worker.
        #[arg(long)]
        pub export_direct_routes: bool,
        /// Print the embedded IPv4 GeoIP dataset SHA-256 and exit.
        #[arg(long)]
        pub geoip_version: bool,
        /// CIDRs forced through the proxy (repeatable or comma-separated).
        #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = client::outbound::ProxyRoutes::parse_network)]
        pub proxy_route: Vec<ipnet::IpNet>,
        #[arg(long, default_value = "cpxy0")]
        pub tun: String,
        /// Which IPv6 to relay: `full` needs a SOCKS5 proxy that can reach IPv6, `direct` relays
        /// only what routes direct, and `off` refuses it.
        #[arg(long, env = "IPV6", value_enum, default_value_t = mobile_engine::Ipv6Mode::Full)]
        pub ipv6: mobile_engine::Ipv6Mode,
        /// The engine sizes TCP segments to this and ignores the peer's MSS, so it must not
        /// exceed the smallest MTU on the path back to clients (1280 behind tailscale0).
        #[arg(long, default_value_t = mobile_engine::DEFAULT_MTU)]
        pub mtu: u16,
        /// Answer DNS with dns_split on this address, for both UDP and TCP. The other --dns
        /// options need it.
        #[arg(long, requires_all = ["dns_upstream", "dns_alternative"])]
        pub dns_listen: Option<std::net::SocketAddr>,
        /// Upstream DNS server: an IP, udp://, tcp://, tls:// or https:// URL. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_listen")]
        pub dns_upstream: Vec<client::dns_split::spec::ServerSpec>,
        /// Alternative DNS server, in the same format as --dns-upstream. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_listen")]
        pub dns_alternative: Vec<client::dns_split::spec::ServerSpec>,
        /// Suffix to try for single-label names, in order, before the bare name. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_listen", value_parser = client::dns_split::search::parse_search_domain)]
        pub dns_search_domain: Vec<hickory_proto::rr::Name>,
        /// The router's own resolver (dnsmasq), in the same format as --dns-upstream. It answers
        /// single-label names, names under --dns-local-domain and reverse lookups of private
        /// addresses, which are not split or cached.
        #[arg(long, requires = "dns_listen")]
        pub dns_local_server: Option<client::dns_split::spec::ServerSpec>,
        /// A domain only the local resolver knows, e.g. `lan`. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_local_server")]
        pub dns_local_domain: Vec<hickory_proto::rr::Name>,
        /// Disable the in-memory DNS answer cache
        #[arg(long, requires = "dns_listen")]
        pub no_dns_cache: bool,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn exported_routes_match_userspace_policy_at_boundaries() {
            let proxy = client::outbound::ProxyRoutes::parse(&[
                "114.0.0.0/8".into(),
                "10.0.0.0/9".into(),
                "223.5.5.5/32".into(),
            ])
            .unwrap();
            let cuts = proxy
                .0
                .iter()
                .filter_map(|net| match net {
                    ipnet::IpNet::V4(n) => Some((u32::from(n.network()), u32::from(n.broadcast()))),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let prefixes = geoip_data::routes::direct_ipv4(&cuts);
            let mut addresses = vec![
                0,
                u32::MAX,
                0x0a000000,
                0x0affffff,
                0x7f000001,
                0xa9fe0001,
                0xac100001,
                0xc0a80001,
            ];
            for bytes in geoip_data::CN.chunks_exact(4) {
                let ip = u32::from_be_bytes(bytes.try_into().unwrap());
                addresses.extend([ip.saturating_sub(1), ip, ip.saturating_add(1)]);
            }
            let mut sample = 42u32;
            for _ in 0..50_000 {
                sample = sample.wrapping_mul(1664525).wrapping_add(1013904223);
                addresses.push(sample);
            }
            for ip in addresses {
                let address = std::net::IpAddr::V4(ip.into());
                let expected = !proxy.contains(Some(address))
                    && (client::outbound::is_private(address)
                        || client::dns_split::is_local_region_ip(address));
                let index = prefixes.partition_point(|&(start, _)| start <= ip);
                let actual = index > 0 && {
                    let (start, prefix) = prefixes[index - 1];
                    (ip as u64) < start as u64 + (1u64 << (32 - prefix))
                };
                assert_eq!(actual, expected, "{address}");
            }
        }

        #[test]
        fn exported_ipv6_routes_match_userspace_at_boundaries() {
            let proxy = client::outbound::ProxyRoutes::parse(&[
                "2400:3200::/33".into(),
                "fc00::/8".into(),
                "::1/128".into(),
            ])
            .unwrap();
            let cuts = proxy
                .0
                .iter()
                .filter_map(|net| match net {
                    ipnet::IpNet::V6(n) => {
                        Some((u128::from(n.network()), u128::from(n.broadcast())))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let prefixes = geoip_data::routes::router_direct_ipv6(&cuts);
            let mut addresses = vec![0, 1, u128::MAX];
            for &(start, prefix) in &prefixes {
                let mask = u128::MAX.checked_shl((128 - prefix) as u32).unwrap_or(0);
                let end = start | !mask;
                addresses.extend([start.saturating_sub(1), start, end, end.saturating_add(1)]);
            }
            for bytes in geoip_data::CN6.chunks_exact(16) {
                let ip = u128::from_be_bytes(bytes.try_into().unwrap());
                addresses.extend([ip.saturating_sub(1), ip, ip.saturating_add(1)]);
            }
            let mut random = 42u128;
            for _ in 0..50_000 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                addresses.push(random);
            }
            for ip in addresses {
                let address = std::net::IpAddr::V6(ip.into());
                let expected = !proxy.contains(Some(address))
                    && (client::outbound::is_private(address)
                        || client::dns_split::is_local_region_ip(address));
                let index = prefixes.partition_point(|&(start, _)| start <= ip);
                let actual = index > 0 && {
                    let (start, prefix) = prefixes[index - 1];
                    let mask = u128::MAX.checked_shl((128 - prefix) as u32).unwrap_or(0);
                    ip & mask == start
                };
                assert_eq!(actual, expected, "{address}");
            }
            assert!(geoip_data::routes::router_direct_ipv6(&[(0, u128::MAX)]).is_empty());
        }

        #[test]
        fn socks5_is_required_except_for_policy_exports() {
            assert!(Options::try_parse_from(["cpxy-router"]).is_err());
            for flag in [
                "--export-direct-routes",
                "--export-direct-routes-v6",
                "--geoip-version",
            ] {
                assert!(Options::try_parse_from(["cpxy-router", flag]).is_ok());
            }
            assert!(
                Options::try_parse_from([
                    "cpxy-router",
                    "--socks5-proxy",
                    "socks5://user:pass@localhost:1080"
                ])
                .is_err()
            );
        }
    }
}
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use clap::Parser;
    use cli::Options;

    tracing_subscriber::fmt::init();
    let options = Options::parse();
    if options.geoip_version {
        println!("{}", geoip_data::IPV4_SHA256);
        return Ok(());
    }
    if options.export_direct_routes {
        use std::io::Write;
        let proxy = options
            .proxy_route
            .iter()
            .filter_map(|net| match net {
                ipnet::IpNet::V4(net) => {
                    Some((u32::from(net.network()), u32::from(net.broadcast())))
                }
                ipnet::IpNet::V6(_) => None,
            })
            .collect::<Vec<_>>();
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        for (addr, prefix) in geoip_data::routes::direct_ipv4(&proxy) {
            writeln!(out, "{}/{prefix}", std::net::Ipv4Addr::from(addr))?;
        }
        out.flush()?;
        return Ok(());
    }
    if options.export_direct_routes_v6 {
        let cuts = options
            .proxy_route
            .iter()
            .filter_map(|net| match net {
                ipnet::IpNet::V6(net) => {
                    Some((u128::from(net.network()), u128::from(net.broadcast())))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        for (addr, prefix) in geoip_data::routes::router_direct_ipv6(&cuts) {
            writeln!(out, "{}/{prefix}", std::net::Ipv6Addr::from(addr))?;
        }
        out.flush()?;
        return Ok(());
    }
    let proxy = options.socks5_proxy.context("--socks5-proxy is required")?;
    let dns = options.dns_listen.map(|listen| mobile_engine::RouterDns {
        listen,
        upstream: options.dns_upstream,
        alternative: options.dns_alternative,
        search_domains: options.dns_search_domain,
        local_server: options.dns_local_server,
        local_domains: options.dns_local_domain,
        cache: !options.no_dns_cache,
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let tun = mobile_engine::linux::create_tun(&options.tun)?;
    let engine = mobile_engine::start_router(
        tun,
        proxy,
        client::outbound::ProxyRoutes(options.proxy_route),
        options.ipv6,
        options.mtu,
        dns,
    )?;
    let result = runtime.block_on(async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        loop {
            tokio::select! {
                _ = term.recv() => return anyhow::Ok(()),
                _ = tokio::signal::ctrl_c() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    anyhow::ensure!(engine.is_running(), "Packet engine stopped");
                }
            }
        }
    });
    engine.stop();
    result
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cpxy-router only runs on Linux");
    std::process::exit(1);
}
