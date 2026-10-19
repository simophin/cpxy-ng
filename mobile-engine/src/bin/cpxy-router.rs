//! Native OpenWrt TUN client; routing and device persistence belong to the init script.
#[cfg(any(target_os = "linux", test))]
mod cli {
    use clap::Parser;

    #[derive(Parser)]
    pub struct Options {
        /// CIDRs forced through the proxy (repeatable or comma-separated).
        #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = client::outbound::ProxyRoutes::parse_network)]
        pub proxy_route: Vec<ipnet::IpNet>,
        #[arg(long, default_value = "cpxy0")]
        pub tun: String,
        /// Which IPv6 to relay: `full` needs a cpxy server that can reach IPv6, `direct` relays
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
        /// Serve the read-only traffic API and offline browser UI on this address.
        /// Disabled unless set. No authentication: use loopback or a trusted management IP.
        #[arg(long, env = "API_LISTEN")]
        pub api_listen: Option<std::net::SocketAddr>,
        // Keep the secret out of process arguments and parser diagnostics.
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use clap::CommandFactory;

        #[test]
        fn api_is_opt_in_and_requires_a_socket_address() {
            Options::command().debug_assert();
            assert!(
                Options::try_parse_from(["cpxy-router"])
                    .unwrap()
                    .api_listen
                    .is_none()
            );
            let options =
                Options::try_parse_from(["cpxy-router", "--api-listen", "127.0.0.1:3010"]).unwrap();
            assert_eq!(options.api_listen.unwrap().to_string(), "127.0.0.1:3010");
            assert!(Options::try_parse_from(["cpxy-router", "--api-listen", "nope"]).is_err());
            let help = Options::command().render_long_help().to_string();
            assert!(help.contains("--api-listen"));
            assert!(help.contains("API_LISTEN"));
        }
    }
}
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use clap::Parser;
    use cli::Options;
    use std::sync::Arc;

    tracing_subscriber::fmt::init();
    let options = Options::parse();
    let server = std::env::var("SERVER")
        .map_err(|_| anyhow::anyhow!("SERVER is required"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid SERVER URL"))?;
    // dns_split asks the servers given with ?via=cpxy through the same server.
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
    // Fail before starting the worker if the explicitly requested API port is unavailable.
    let api_listener = options
        .api_listen
        .map(|address| runtime.block_on(tokio::net::TcpListener::bind(address)))
        .transpose()
        .context("Cannot bind traffic API listener")?;
    let tun = mobile_engine::linux::create_tun(&options.tun)?;
    let engine = Arc::new(mobile_engine::start_router(
        tun,
        server,
        client::outbound::ProxyRoutes(options.proxy_route),
        options.ipv6,
        options.mtu,
        dns,
        Arc::new(()),
    )?);
    let result = runtime.block_on(async {
        let api = async {
            match api_listener {
                Some(listener) => mobile_engine::traffic_api::serve(listener, engine.clone()).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(api);
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        loop {
            tokio::select! {
                result = &mut api => {
                    result.context("Traffic API stopped")?;
                    anyhow::bail!("Traffic API stopped unexpectedly");
                }
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
