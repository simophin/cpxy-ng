//! Answers DNS sent to the virtual DNS server with `dns_split`.

use crate::config::SINKHOLE_ADDR;
use anyhow::Context;
use blocklist_data::Blocklist;
use client::dns_split::cache::{CacheOptions, DnsCache};
use client::dns_split::is_local_region_ip;
use client::dns_split::policy::Racer;
use client::dns_split::server::{DnsSplitHandler, encode_udp, serve_tcp_connection};
use client::dns_split::spec::ServerSpec;
use client::dns_split::upstream::{DnsUpstream, Group, HickoryUpstream, tls_client_config};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use hickory_proto::op::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Clients usually send each query from a fresh port, so a UDP flow is done once it goes quiet.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_OPTIONS: CacheOptions = CacheOptions {
    min_ttl: 30,
    max_ttl: 86400,
    negative_ttl: 300,
};

pub async fn handler(
    upstream: &[ServerSpec],
    alternative: &[ServerSpec],
    blocklist: Option<&'static Blocklist>,
) -> anyhow::Result<DnsSplitHandler> {
    let tls_config = tls_client_config()?;
    let mut servers: Vec<(Group, Arc<dyn DnsUpstream>)> = Vec::new();
    for (group, specs) in [
        (Group::Upstream, upstream),
        (Group::Alternative, alternative),
    ] {
        for spec in specs {
            let server = HickoryUpstream::new(spec, tls_config.clone(), QUERY_TIMEOUT)
                .await
                .with_context(|| format!("Error setting up {group} DNS server {spec}"))?;
            tracing::info!("Using {group} DNS server {}", server.name());
            servers.push((group, Arc::new(server)));
        }
    }

    // In memory: the app has no use for answers across restarts, and the cache only lives as
    // long as the tunnel.
    let cache = DnsCache::open_in_memory(CACHE_OPTIONS).await?;
    let handler = DnsSplitHandler::new(
        Racer::new(servers, QUERY_TIMEOUT, is_local_region_ip),
        Some(Arc::new(cache)),
    );
    let Some(blocklist) = blocklist else {
        return Ok(handler);
    };
    tracing::info!("Blocking {} domains", blocklist.len());
    Ok(handler.with_sinkhole(blocklist, SINKHOLE_ADDR))
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
