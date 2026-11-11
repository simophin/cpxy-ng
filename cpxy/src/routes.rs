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

    /// Print IPv6 prefixes instead of IPv4
    #[arg(long)]
    ipv6: bool,
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
            Command::CnDirect(args) if args.ipv6 => {
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
