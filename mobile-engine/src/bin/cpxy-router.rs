//! Native OpenWrt TUN client; routing and device persistence belong to the init script.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use clap::Parser;
    use mobile_engine::{EventListener, OutboundEvent};
    use std::sync::Arc;

    #[derive(Parser)]
    struct Options {
        /// IPv4 CIDRs forced through the proxy (repeatable or comma-separated).
        #[arg(long, env = "PROXY_ROUTES", value_delimiter = ',', value_parser = client::outbound::ProxyRoutes::parse_network)]
        proxy_route: Vec<ipnet::Ipv4Net>,
        #[arg(long, default_value = "cpxy0")]
        tun: String,
        /// The engine sizes TCP segments to this and ignores the peer's MSS, so it must not
        /// exceed the smallest MTU on the path back to clients (1280 behind tailscale0).
        #[arg(long, default_value_t = mobile_engine::DEFAULT_MTU)]
        mtu: u16,
        /// Answer DNS with dns_split on this address, for both UDP and TCP. The other --dns
        /// options need it.
        #[arg(long, requires_all = ["dns_upstream", "dns_alternative"])]
        dns_listen: Option<std::net::SocketAddr>,
        /// Upstream DNS server: an IP, udp://, tcp://, tls:// or https:// URL. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_listen")]
        dns_upstream: Vec<client::dns_split::spec::ServerSpec>,
        /// Alternative DNS server, in the same format as --dns-upstream. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_listen")]
        dns_alternative: Vec<client::dns_split::spec::ServerSpec>,
        /// Suffix to try for single-label names, in order, before the bare name. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_listen", value_parser = client::dns_split::search::parse_search_domain)]
        dns_search_domain: Vec<hickory_proto::rr::Name>,
        /// The router's own resolver (dnsmasq), in the same format as --dns-upstream. It answers
        /// single-label names, names under --dns-local-domain and reverse lookups of private
        /// addresses, which are not split or cached.
        #[arg(long, requires = "dns_listen")]
        dns_local_server: Option<client::dns_split::spec::ServerSpec>,
        /// A domain only the local resolver knows, e.g. `lan`. Repeatable.
        #[arg(long, value_delimiter = ',', requires = "dns_local_server")]
        dns_local_domain: Vec<hickory_proto::rr::Name>,
        /// Disable the in-memory DNS answer cache
        #[arg(long, requires = "dns_listen")]
        no_dns_cache: bool,
        // Keep the secret out of process arguments and parser diagnostics.
    }
    struct Listener;
    impl EventListener for Listener {
        fn on_outbound_event(&self, _: OutboundEvent) {}
    }
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
    let tun = mobile_engine::linux::create_tun(&options.tun)?;
    let engine = mobile_engine::start_router(
        tun,
        server,
        client::outbound::ProxyRoutes(options.proxy_route),
        options.mtu,
        dns,
        Arc::new(Listener),
    )?;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
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
