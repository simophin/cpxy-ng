//! The DNS split: upstream resolvers are asked directly, alternative ones through the tunnel.

use crate::packet::{INJECT_PORTS, udp_v4};
use crate::shim::Datagrams;
use anyhow::{Context, ensure};
use client::dns_split::cache::{CacheOptions, DnsCache, purge_expired_every};
use client::dns_split::is_local_region_ip;
use client::dns_split::policy::Racer;
use client::dns_split::server::DnsSplitHandler;
use client::dns_split::upstream::{DnsUpstream, Group, HickoryUpstream, tls_client_config};
use client::ipv6::Ipv6Mode;
use futures::future::BoxFuture;
use hickory_proto::op::{Edns, Message};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Fits a tunnel packet with room for the IPv4 and UDP headers, without fragmenting.
const EDNS_PAYLOAD: u16 = (rtp_l3::MTU - 20 - 8) as u16;
const CACHE_OPTIONS: CacheOptions = CacheOptions {
    min_ttl: 30,
    max_ttl: 86400,
    negative_ttl: 300,
};
const PURGE_INTERVAL: Duration = Duration::from_secs(300);

/// Must run on the engine's runtime, which then purges the cache's expired answers.
pub async fn handler(
    settings: &crate::profile::Settings,
    tunnel: Arc<TunnelQueries>,
) -> anyhow::Result<DnsSplitHandler> {
    let tls_config = tls_client_config()?;
    let mut servers: Vec<(Group, Arc<dyn DnsUpstream>)> = Vec::new();
    for spec in &settings.dns_upstream {
        // The app is outside its own VPN, so these sockets go direct.
        let server = HickoryUpstream::new(spec, tls_config.clone(), QUERY_TIMEOUT, None)
            .await
            .with_context(|| format!("Error setting up upstream DNS server {spec}"))?;
        tracing::info!("Using upstream DNS server {}", server.name());
        servers.push((Group::Upstream, Arc::new(server)));
    }
    for &server in &settings.dns_alternative {
        tracing::info!("Using alternative DNS server {server} through the tunnel");
        servers.push((
            Group::Alternative,
            Arc::new(TunnelResolver {
                name: format!("{server} (tunnel)"),
                server,
                queries: tunnel.clone(),
            }),
        ));
    }
    let cache = Arc::new(DnsCache::open_in_memory(CACHE_OPTIONS).await?);
    tokio::spawn(purge_expired_every(cache.clone(), PURGE_INTERVAL));
    let mut handler = DnsSplitHandler::new(
        Racer::new(servers, QUERY_TIMEOUT, is_local_region_ip),
        Some(cache),
    )
    // The tunnel carries IPv4 only.
    .with_ipv6(Ipv6Mode::Off)
    .with_search_domains(settings.dns_search_domains.clone());
    if let Some(spec) = &settings.dns_local_server {
        let local = HickoryUpstream::new(spec, tls_config, QUERY_TIMEOUT, None).await?;
        handler = handler.with_local(Arc::new(local), settings.dns_local_domains.clone());
    }
    Ok(handler)
}

/// DNS queries the engine itself sends through the tunnel, as UDP from the tunnel address.
pub struct TunnelQueries {
    own: Ipv4Addr,
    tunnel: Datagrams,
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    next: u16,
    replies: HashMap<u16, oneshot::Sender<Vec<u8>>>,
}

impl TunnelQueries {
    pub fn new(own: Ipv4Addr, tunnel: Datagrams) -> Self {
        Self {
            own,
            tunnel,
            pending: Default::default(),
        }
    }

    /// Hands a reply that arrived on `port` to its query.
    pub fn deliver(&self, port: u16, payload: &[u8]) {
        if let Some(reply) = self.pending.lock().unwrap().replies.remove(&port) {
            let _ = reply.send(payload.to_vec());
        }
    }

    async fn query(&self, server: Ipv4Addr, mut request: Message) -> anyhow::Result<Message> {
        let payload = request
            .extensions()
            .as_ref()
            .map_or(EDNS_PAYLOAD, |edns| edns.max_payload().min(EDNS_PAYLOAD));
        let mut edns = request.extensions().clone().unwrap_or_else(Edns::new);
        edns.set_max_payload(payload);
        request.set_edns(edns);
        let (tx, rx) = oneshot::channel();
        let port = self.reserve(tx)?;
        let _release = Release(self, port);
        let packet = udp_v4(self.own, port, server, 53, &request.to_vec()?);
        self.tunnel.send(&packet).await?;
        let reply = tokio::time::timeout(QUERY_TIMEOUT, rx)
            .await
            .context("Timed out")??;
        let response = Message::from_vec(&reply)?;
        ensure!(response.id() == request.id(), "Mismatched DNS reply");
        Ok(response)
    }

    /// A source port not used by another query in flight.
    fn reserve(&self, reply: oneshot::Sender<Vec<u8>>) -> anyhow::Result<u16> {
        let mut pending = self.pending.lock().unwrap();
        let span = INJECT_PORTS.end - INJECT_PORTS.start;
        for _ in 0..span {
            let port = INJECT_PORTS.start + pending.next % span;
            pending.next = pending.next.wrapping_add(1);
            if let std::collections::hash_map::Entry::Vacant(entry) = pending.replies.entry(port) {
                entry.insert(reply);
                return Ok(port);
            }
        }
        anyhow::bail!("Too many DNS queries in flight")
    }
}

struct Release<'a>(&'a TunnelQueries, u16);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().replies.remove(&self.1);
    }
}

struct TunnelResolver {
    name: String,
    server: Ipv4Addr,
    queries: Arc<TunnelQueries>,
}

impl DnsUpstream for TunnelResolver {
    fn name(&self) -> &str {
        &self.name
    }

    fn query(&self, request: Message) -> BoxFuture<'_, anyhow::Result<Message>> {
        Box::pin(self.queries.query(self.server, request))
    }
}
