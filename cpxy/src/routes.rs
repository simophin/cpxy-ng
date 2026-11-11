//! `cpxy routes`: the CN split routing policy as IP prefixes, e.g. for a router's nftables sets.

use client::outbound::ProxyRoutes;
use ipnet::IpNet;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Print the prefixes that connect directly, one per line: Chinese and private addresses,
    /// minus --proxy-route
    CnDirect(CnDirectArgs),
    /// Print the SHA-256 of the embedded IPv4 GeoIP dataset
    GeoipVersion,
}

#[derive(clap::Args)]
pub struct CnDirectArgs {
    /// CIDRs that go through the proxy even when they are in China. Repeatable or
    /// comma-separated.
    #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = ProxyRoutes::parse_network)]
    proxy_route: Vec<IpNet>,

    /// Which addresses to print
    #[arg(long, value_enum, default_value_t = Family::Ipv4)]
    family: Family,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Family {
    Ipv4,
    Ipv6,
}

impl Command {
    pub fn run(self) -> anyhow::Result<()> {
        match self.print() {
            // The reader stopped early, as `| head` does.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            r => Ok(r?),
        }
    }

    fn print(self) -> std::io::Result<()> {
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        match self {
            Command::GeoipVersion => writeln!(out, "{}", geoip_data::IPV4_SHA256)?,
            Command::CnDirect(args) if args.family == Family::Ipv6 => {
                let proxy: Vec<_> = args
                    .proxy_route
                    .iter()
                    .filter_map(|net| match net {
                        IpNet::V6(net) => {
                            Some((u128::from(net.network()), u128::from(net.broadcast())))
                        }
                        IpNet::V4(_) => None,
                    })
                    .collect();
                for (addr, prefix) in geoip_data::routes::router_direct_ipv6(&proxy) {
                    writeln!(out, "{}/{prefix}", Ipv6Addr::from(addr))?;
                }
            }
            Command::CnDirect(args) => {
                let proxy: Vec<_> = args
                    .proxy_route
                    .iter()
                    .filter_map(|net| match net {
                        IpNet::V4(net) => {
                            Some((u32::from(net.network()), u32::from(net.broadcast())))
                        }
                        IpNet::V6(_) => None,
                    })
                    .collect();
                for (addr, prefix) in geoip_data::routes::direct_ipv4(&proxy) {
                    writeln!(out, "{}/{prefix}", Ipv4Addr::from(addr))?;
                }
            }
        }
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use client::dns_split::routing::should_proxy;

    /// The printed prefixes are exactly the addresses the resolvers' and the clients' split
    /// routing sends directly.
    #[test]
    fn direct_ipv4_matches_the_userspace_policy_at_boundaries() {
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
            let expected = !should_proxy(&proxy, Some(address));
            let index = prefixes.partition_point(|&(start, _)| start <= ip);
            let actual = index > 0 && {
                let (start, prefix) = prefixes[index - 1];
                (ip as u64) < start as u64 + (1u64 << (32 - prefix))
            };
            assert_eq!(actual, expected, "{address}");
        }
    }
}
