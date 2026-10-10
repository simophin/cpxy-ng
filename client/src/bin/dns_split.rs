use anyhow::Context;
use clap::Parser;
use client::dns_split::cache::{CacheOptions, DnsCache, unix_now};
use client::dns_split::is_local_region_ip;
use client::dns_split::policy::Racer;
use client::dns_split::server::{DnsSplitHandler, serve_tcp, serve_udp};
use client::dns_split::spec::ServerSpec;
use client::dns_split::upstream::{DnsUpstream, Group, HickoryUpstream, tls_client_config};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};

const PURGE_INTERVAL: Duration = Duration::from_secs(300);

/// A DNS forwarder that races queries across upstream and alternative servers. An answer whose
/// addresses are all in the local region wins; otherwise the alternative servers' answer is used.
#[derive(clap::Parser)]
struct CliOptions {
    /// The address to listen on, for both UDP and TCP
    #[clap(long, env, default_value = "0.0.0.0:53")]
    listen: SocketAddr,

    /// Upstream DNS server: an IP, udp://, tcp://, tls:// or https:// URL. Repeatable.
    #[clap(long, env, required = true, value_delimiter = ',')]
    upstream: Vec<ServerSpec>,

    /// Alternative DNS server, in the same format as --upstream. Repeatable.
    #[clap(long, env, required = true, value_delimiter = ',')]
    alternative: Vec<ServerSpec>,

    /// How long to wait for answers to a query, in seconds
    #[clap(long, env, default_value_t = 5)]
    timeout: u64,

    /// The SQLite file to cache answers in
    #[clap(long, env, default_value = "dns-split-cache.sqlite")]
    cache_db: PathBuf,

    /// Disable the answer cache
    #[clap(long, env)]
    no_cache: bool,

    /// Minimum time to cache an answer, in seconds
    #[clap(long, env, default_value_t = 30)]
    cache_min_ttl: u32,

    /// Maximum time to cache an answer, in seconds
    #[clap(long, env, default_value_t = 86400)]
    cache_max_ttl: u32,

    /// Maximum time to cache a negative answer (no such name, or no records), in seconds
    #[clap(long, env, default_value_t = 300)]
    cache_negative_ttl: u32,

    /// Answer names on the built-in ad blocklist with this address, which the network should
    /// drop silently. Without it, nothing is blocked.
    #[clap(long, env)]
    sinkhole: Option<Ipv4Addr>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let options = CliOptions::parse();
    let timeout = Duration::from_secs(options.timeout);
    let tls_config = tls_client_config()?;

    let mut servers: Vec<(Group, Arc<dyn DnsUpstream>)> = Vec::new();
    for (group, specs) in [
        (Group::Upstream, &options.upstream),
        (Group::Alternative, &options.alternative),
    ] {
        for spec in specs {
            let server = HickoryUpstream::new(spec, tls_config.clone(), timeout)
                .await
                .with_context(|| format!("Error setting up {group} server {spec}"))?;
            tracing::info!("Using {group} server {}", server.name());
            servers.push((group, Arc::new(server)));
        }
    }

    let cache = if options.no_cache {
        None
    } else {
        let cache = Arc::new(
            DnsCache::open(
                &options.cache_db,
                CacheOptions {
                    min_ttl: options.cache_min_ttl,
                    max_ttl: options.cache_max_ttl,
                    negative_ttl: options.cache_negative_ttl,
                },
            )
            .await?,
        );
        tracing::info!("Caching answers in {}", options.cache_db.display());
        tokio::spawn(purge_expired(cache.clone()));
        Some(cache)
    };

    let mut handler = DnsSplitHandler::new(Racer::new(servers, timeout, is_local_region_ip), cache);
    if let Some(sinkhole) = options.sinkhole {
        let blocklist = &blocklist_data::BASELINE;
        tracing::info!("Blocking {} domains with {sinkhole}", blocklist.len());
        handler = handler.with_sinkhole(blocklist, sinkhole);
    }
    let handler = Arc::new(handler);

    let udp = UdpSocket::bind(options.listen)
        .await
        .with_context(|| format!("Error binding UDP {}", options.listen))?;
    let tcp = TcpListener::bind(options.listen)
        .await
        .with_context(|| format!("Error binding TCP {}", options.listen))?;
    tracing::info!("Listening on {}", options.listen);

    tokio::select! {
        r = serve_udp(udp, handler.clone()) => r,
        r = serve_tcp(tcp, handler) => r,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Shutting down");
            Ok(())
        }
    }
}

async fn purge_expired(cache: Arc<DnsCache>) {
    loop {
        tokio::time::sleep(PURGE_INTERVAL).await;
        match cache.purge_expired(unix_now()).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!("Purged {n} expired cache entries"),
            Err(e) => tracing::warn!("Error purging DNS cache: {e:?}"),
        }
    }
}
