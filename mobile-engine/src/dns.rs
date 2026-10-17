//! Answers DNS sent to the virtual DNS server, or to a router's DNS sockets, with `dns_split`.

use anyhow::Context;
use client::dns_split::cache::{CacheOptions, DnsCache, purge_expired_every};
use client::dns_split::is_local_region_ip;
use client::dns_split::policy::Racer;
use client::dns_split::server::{
    DnsSplitHandler, encode_udp, serve_tcp, serve_tcp_connection, serve_udp,
};
use client::dns_split::spec::ServerSpec;
use client::dns_split::upstream::{
    Dialer, DnsUpstream, Group, HickoryUpstream, OutboundDialer, tls_client_config,
};
use client::ipv6::Ipv6Mode;
use client::outbound::ProtocolOutbound;
use client::protocol_config::Config as ServerConfig;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use hickory_proto::op::Message;
use hickory_proto::rr::Name;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinSet;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Clients usually send each query from a fresh port, so a UDP flow is done once it goes quiet.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_OPTIONS: CacheOptions = CacheOptions {
    min_ttl: 30,
    max_ttl: 86400,
    negative_ttl: 300,
};
const PURGE_INTERVAL: Duration = Duration::from_secs(300);

/// The servers `dns_split` asks, and how it answers.
pub struct Specs<'a> {
    pub upstream: &'a [ServerSpec],
    pub alternative: &'a [ServerSpec],
    pub search_domains: &'a [Name],
    /// The network's own resolver and the domains only it knows, as in
    /// [`DnsSplitHandler::with_local`].
    pub local: Option<(&'a ServerSpec, &'a [Name])>,
    pub cache: bool,
    pub ipv6: Ipv6Mode,
}

/// Servers given with `?via=cpxy` are reached through `server`, without the routing that
/// decides between direct and the cpxy server for the device's flows. Must run on the engine's
/// runtime, which then purges the cache's expired answers.
pub async fn handler(specs: &Specs<'_>, server: &ServerConfig) -> anyhow::Result<DnsSplitHandler> {
    let tls_config = tls_client_config()?;
    let cpxy: Arc<dyn Dialer> =
        Arc::new(OutboundDialer(Arc::new(ProtocolOutbound(server.clone()))));
    let new_upstream = async |spec: &ServerSpec| {
        HickoryUpstream::new(spec, tls_config.clone(), QUERY_TIMEOUT, Some(cpxy.clone())).await
    };
    let mut servers: Vec<(Group, Arc<dyn DnsUpstream>)> = Vec::new();
    for (group, group_specs) in [
        (Group::Upstream, specs.upstream),
        (Group::Alternative, specs.alternative),
    ] {
        for spec in group_specs {
            let server = new_upstream(spec)
                .await
                .with_context(|| format!("Error setting up {group} DNS server {spec}"))?;
            tracing::info!("Using {group} DNS server {}", server.name());
            servers.push((group, Arc::new(server)));
        }
    }

    // In memory: answers are not kept across restarts, and the cache only lives as long as the
    // engine.
    let cache = if specs.cache {
        let cache = Arc::new(DnsCache::open_in_memory(CACHE_OPTIONS).await?);
        tokio::spawn(purge_expired_every(cache.clone(), PURGE_INTERVAL));
        Some(cache)
    } else {
        None
    };
    let mut handler = DnsSplitHandler::new(
        Racer::new(servers, QUERY_TIMEOUT, is_local_region_ip),
        cache,
    )
    .with_search_domains(specs.search_domains.to_vec())
    .with_ipv6(specs.ipv6);
    if let Some((spec, domains)) = specs.local {
        let server = new_upstream(spec)
            .await
            .with_context(|| format!("Error setting up local DNS server {spec}"))?;
        tracing::info!("Using local DNS server {} for local names", server.name());
        let domains = domains.iter().map(|domain| {
            let mut domain = domain.clone();
            domain.set_fqdn(true);
            domain
        });
        handler = handler.with_local(Arc::new(server), domains.collect());
    }
    Ok(handler)
}

/// A router's DNS sockets: UDP and TCP on one address.
pub(crate) struct Listeners {
    udp: UdpSocket,
    tcp: TcpListener,
}

impl Listeners {
    pub async fn bind(addr: SocketAddr) -> anyhow::Result<Self> {
        let udp = UdpSocket::bind(addr)
            .await
            .with_context(|| format!("Cannot bind DNS on UDP {addr}"))?;
        let tcp = TcpListener::bind(addr)
            .await
            .with_context(|| format!("Cannot bind DNS on TCP {addr}"))?;
        tracing::info!("Answering DNS on {addr}");
        Ok(Self { udp, tcp })
    }

    pub fn spawn(self, handler: Arc<DnsSplitHandler>, tasks: &mut JoinSet<anyhow::Result<()>>) {
        tasks.spawn(serve_udp(self.udp, handler.clone()));
        tasks.spawn(serve_tcp(self.tcp, handler));
    }
}

/// Answers each datagram of one UDP flow, concurrently: resolvers often send the A and AAAA
/// queries from the same port at once.
pub async fn serve_udp_flow(
    mut flow: impl AsyncRead + AsyncWrite + Unpin,
    handler: Arc<DnsSplitHandler>,
) {
    let mut buf = vec![0u8; 65535];
    let mut pending = FuturesUnordered::new();
    loop {
        tokio::select! {
            read = tokio::time::timeout(UDP_IDLE_TIMEOUT, flow.read(&mut buf)) => {
                let n = match read {
                    Ok(Ok(n)) if n > 0 => n,
                    _ => break,
                };
                let request = match Message::from_vec(&buf[..n]) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::debug!("Invalid DNS request: {e:?}");
                        continue;
                    }
                };
                let handler = handler.clone();
                pending.push(async move {
                    let response = handler.handle(&request).await;
                    encode_udp(&request, &response)
                });
            }
            Some(reply) = pending.next() => write_reply(&mut flow, reply).await,
        }
    }

    while let Some(reply) = pending.next().await {
        write_reply(&mut flow, reply).await;
    }
}

async fn write_reply(flow: &mut (impl AsyncWrite + Unpin), reply: anyhow::Result<Vec<u8>>) {
    match reply {
        Ok(bytes) => {
            if let Err(e) = flow.write_all(&bytes).await {
                tracing::debug!("Error writing DNS reply: {e:?}");
            }
        }
        Err(e) => tracing::warn!("{e:?}"),
    }
}

pub async fn serve_tcp_flow(
    flow: impl AsyncRead + AsyncWrite + Unpin,
    handler: Arc<DnsSplitHandler>,
) {
    if let Err(e) = serve_tcp_connection(flow, &handler).await {
        tracing::debug!("DNS over TCP ended: {e:?}");
    }
}
