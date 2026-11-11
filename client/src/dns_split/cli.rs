//! `cpxy dns`: a split DNS server.

use super::cache::{CacheOptions, DnsCache, purge_expired_every};
use super::is_local_region_ip;
use super::policy::Racer;
use super::server::{DnsSplitHandler, serve_tcp, serve_udp};
use super::spec::ServerSpec;
use super::upstream::{
    Dialer, DnsUpstream, Group, HickoryUpstream, OutboundDialer, tls_client_config,
};
use crate::ipv6::Ipv6Mode;
use crate::outbound::{ProtocolOutbound, Socks5Outbound};
use crate::protocol_config::Config as ServerConfig;
use anyhow::Context;
use hickory_proto::rr::Name;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};

const PURGE_INTERVAL: Duration = Duration::from_secs(300);

/// Where answers are cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cache {
    Memory,
    Off,
    File(PathBuf),
}

impl FromStr for Cache {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "memory" => Cache::Memory,
            "off" => Cache::Off,
            path => Cache::File(path.into()),
        })
    }
}

/// A DNS forwarder that races queries across upstream and alternative servers. An answer with any
/// address in the local region from an upstream wins, keeping just those addresses. Alternative
/// answers wait until upstreams finish without a local answer, or the query deadline expires.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The address to listen on, for both UDP and TCP
    #[arg(long, env = "LISTEN", default_value = "0.0.0.0:53")]
    listen: SocketAddr,

    /// Upstream DNS server: an IP, udp://, tcp://, tls:// or https:// URL. Repeatable or
    /// comma-separated.
    #[arg(long, env = "UPSTREAM", required = true, value_delimiter = ',')]
    upstream: Vec<ServerSpec>,

    /// Alternative DNS server, in the same format as --upstream. Repeatable or comma-separated.
    #[arg(long, env = "ALTERNATIVE", required = true, value_delimiter = ',')]
    alternative: Vec<ServerSpec>,

    /// The cpxy server URL with its key, `https://:<key>@<host>:<port>`, that the servers given
    /// with `?via=cpxy` are reached through
    #[arg(
        long,
        env = "SERVER",
        hide_env_values = true,
        conflicts_with = "socks5_proxy"
    )]
    server: Option<ServerConfig>,

    /// A plaintext, no-auth SOCKS5 proxy (e.g. `cpxy client`'s) that the servers given with
    /// `?via=cpxy` are reached through, instead of --server
    #[arg(long, env = "SOCKS5_PROXY")]
    socks5_proxy: Option<SocketAddr>,

    /// How long to wait for answers to a query, in seconds
    #[arg(long, env = "TIMEOUT", default_value_t = 5)]
    timeout: u64,

    /// Where to cache answers: `memory`, `off`, or the path of a SQLite file, which keeps them
    /// across restarts
    #[arg(long, env = "CACHE", default_value = "memory")]
    cache: Cache,

    /// Minimum time to cache an answer, in seconds
    #[arg(long, env = "CACHE_MIN_TTL", default_value_t = 30)]
    cache_min_ttl: u32,

    /// Maximum time to cache an answer, in seconds
    #[arg(long, env = "CACHE_MAX_TTL", default_value_t = 86400)]
    cache_max_ttl: u32,

    /// Maximum time to cache a negative answer (no such name, or no records), in seconds
    #[arg(long, env = "CACHE_NEGATIVE_TTL", default_value_t = 300)]
    cache_negative_ttl: u32,

    /// The local network's own resolver (e.g. dnsmasq), in the same format as --upstream. It
    /// answers single-label names, names under --local-domain and reverse lookups of private
    /// addresses, which are not split or cached.
    #[arg(long, env = "LOCAL_SERVER")]
    local_server: Option<ServerSpec>,

    /// A domain only the local resolver knows, e.g. `lan`. Repeatable or comma-separated; needs
    /// --local-server.
    #[arg(
        long,
        env = "LOCAL_DOMAINS",
        value_delimiter = ',',
        requires = "local_server"
    )]
    local_domain: Vec<Name>,

    /// Suffix to try for single-label names, in order, before the bare name. Repeatable or
    /// comma-separated.
    #[arg(long, env = "SEARCH_DOMAINS", value_delimiter = ',', value_parser = super::search::parse_search_domain)]
    search_domain: Vec<Name>,

    /// IPv6 answer policy: off returns empty AAAA answers; direct retains local-region AAAA only.
    #[arg(long, env = "IPV6", value_enum, default_value = "off")]
    ipv6: Ipv6Mode,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let timeout = Duration::from_secs(args.timeout);
    let tls_config = tls_client_config()?;
    let cpxy: Option<Arc<dyn Dialer>> = match (&args.server, args.socks5_proxy) {
        (Some(server), _) => {
            tracing::info!("Using cpxy server {server:?} for the servers via cpxy");
            Some(Arc::new(OutboundDialer(Arc::new(ProtocolOutbound(
                server.clone(),
            )))))
        }
        (None, Some(proxy)) => {
            tracing::info!("Using SOCKS5 proxy {proxy} for the servers via cpxy");
            Some(Arc::new(OutboundDialer(Arc::new(Socks5Outbound(proxy)))))
        }
        (None, None) => None,
    };

    let mut servers: Vec<(Group, Arc<dyn DnsUpstream>)> = Vec::new();
    for (group, specs) in [
        (Group::Upstream, &args.upstream),
        (Group::Alternative, &args.alternative),
    ] {
        for spec in specs {
            let server = HickoryUpstream::new(spec, tls_config.clone(), timeout, cpxy.clone())
                .await
                .with_context(|| format!("Error setting up {group} server {spec}"))?;
            tracing::info!("Using {group} server {}", server.name());
            servers.push((group, Arc::new(server)));
        }
    }

    let options = CacheOptions {
        min_ttl: args.cache_min_ttl,
        max_ttl: args.cache_max_ttl,
        negative_ttl: args.cache_negative_ttl,
    };
    let cache = match &args.cache {
        Cache::Off => None,
        Cache::Memory => Some(DnsCache::open_in_memory(options).await?),
        Cache::File(path) => Some(DnsCache::open(path, options).await?),
    };
    let cache = cache.map(|cache| {
        tracing::info!(cache = ?args.cache, "Caching answers");
        let cache = Arc::new(cache);
        tokio::spawn(purge_expired_every(cache.clone(), PURGE_INTERVAL));
        cache
    });

    let mut handler = DnsSplitHandler::new(Racer::new(servers, timeout, is_local_region_ip), cache)
        .with_search_domains(args.search_domain)
        .with_ipv6(args.ipv6);
    if let Some(spec) = &args.local_server {
        let server = HickoryUpstream::new(spec, tls_config.clone(), timeout, cpxy.clone())
            .await
            .with_context(|| format!("Error setting up local server {spec}"))?;
        tracing::info!("Using local server {} for local names", server.name());
        let domains = args.local_domain.iter().map(|domain| {
            let mut domain = domain.clone();
            domain.set_fqdn(true);
            domain
        });
        handler = handler.with_local(Arc::new(server), domains.collect());
    }
    let handler = Arc::new(handler);

    let udp = UdpSocket::bind(args.listen)
        .await
        .with_context(|| format!("Error binding UDP {}", args.listen))?;
    let tcp = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("Error binding TCP {}", args.listen))?;
    tracing::info!("Listening on {}", args.listen);

    tokio::select! {
        r = serve_udp(udp, handler.clone()) => r,
        r = serve_tcp(tcp, handler) => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_values() {
        assert_eq!("memory".parse::<Cache>().unwrap(), Cache::Memory);
        assert_eq!("off".parse::<Cache>().unwrap(), Cache::Off);
        assert_eq!(
            "/data/dns.sqlite".parse::<Cache>().unwrap(),
            Cache::File("/data/dns.sqlite".into())
        );
    }
}
