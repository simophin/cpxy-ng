//! Answers DNS sent to the virtual DNS server, or to a router's DNS sockets, with `dns_split`.

use anyhow::Context;
use client::dns_split::cache::{CacheOptions, DnsCache, purge_expired_every};
use client::dns_split::is_local_region_ip;
use client::dns_split::policy::Racer;
use client::dns_split::server::{
    DnsSplitHandler, encode_udp, serve_tcp, serve_tcp_connection, serve_udp,
};
use client::dns_split::spec::{Destination, Endpoint, ServerSpec, Via};
use client::dns_split::upstream::{
    Dialer, DnsUpstream, Group, HickoryUpstream, OutboundDialer, tls_client_config,
};
use client::ipv6::Ipv6Mode;
use client::outbound::{ProtocolOutbound, ProxyRoutes, Socks5Outbound};
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
    build_handler(
        specs,
        Arc::new(OutboundDialer(Arc::new(ProtocolOutbound(server.clone())))),
        None,
    )
    .await
}

/// Router sockets bypass ingress policy rules, so classify resolver destinations explicitly.
/// Proxied UDP DNS is upgraded to TCP; the LAN-facing listener still accepts UDP and TCP.
pub async fn router_handler(
    specs: &Specs<'_>,
    proxy: SocketAddr,
    routes: &ProxyRoutes,
) -> anyhow::Result<DnsSplitHandler> {
    build_handler(
        specs,
        Arc::new(OutboundDialer(Arc::new(Socks5Outbound(proxy)))),
        Some(routes),
    )
    .await
}

async fn router_spec(spec: &ServerSpec, routes: &ProxyRoutes) -> anyhow::Result<ServerSpec> {
    let mut spec = spec.clone();
    // Legacy explicit proxy specs stay supported; the dialer is now SOCKS5.
    if spec.via == Via::Cpxy {
        return Ok(spec);
    }
    let Destination::Addr(address) = spec.resolve().await? else {
        anyhow::bail!("DNS address must resolve before classification");
    };
    let proxy = crate::routing::route(routes, Some(address.ip())) == crate::routing::Route::Proxy;
    spec.via = if proxy { Via::Cpxy } else { Via::Direct };
    match &mut spec.endpoint {
        Endpoint::Udp(_) if proxy => spec.endpoint = Endpoint::Tcp(address),
        Endpoint::Tls { ip, .. } | Endpoint::Https { ip, .. } => *ip = Some(address.ip()),
        _ => {}
    }
    Ok(spec)
}

async fn build_handler(
    specs: &Specs<'_>,
    cpxy: Arc<dyn Dialer>,
    routes: Option<&ProxyRoutes>,
) -> anyhow::Result<DnsSplitHandler> {
    let tls_config = tls_client_config()?;
    let new_upstream = async |spec: &ServerSpec| {
        let spec = match routes {
            Some(routes) => router_spec(spec, routes).await?,
            None => spec.clone(),
        };
        HickoryUpstream::new(&spec, tls_config.clone(), QUERY_TIMEOUT, Some(cpxy.clone())).await
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
        let server = if routes.is_some() {
            let mut local = spec.clone();
            local.via = Via::Direct;
            HickoryUpstream::new(&local, tls_config.clone(), QUERY_TIMEOUT, None).await
        } else {
            new_upstream(spec).await
        }
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

#[cfg(test)]
mod router_tests {
    use super::*;
    use client::handshaker::Handshaker;
    use client::socks_proxy_server::{ProxyRequest, SocksProxyHandshaker};
    use hickory_proto::op::{MessageType, Query};
    use hickory_proto::rr::RecordType;

    #[tokio::test]
    async fn resolver_transport_follows_destination_and_overrides_in_both_families() {
        let routes = ProxyRoutes::parse(&["223.5.5.5/32".into(), "2400:3200::/32".into()]).unwrap();
        for (input, proxy, tcp) in [
            ("114.114.114.114", false, false),
            ("8.8.8.8", true, true),
            ("223.5.5.5", true, true),
            ("[2400:3200::1]:53", true, true),
            ("[240e::1]:53", false, false),
            ("[2001:4860:4860::8888]:53", true, true),
            ("[fd00::1]:53", false, false),
            ("https://dns.google?ip=8.8.8.8", true, false),
        ] {
            let spec = router_spec(&input.parse().unwrap(), &routes).await.unwrap();
            assert_eq!(spec.via == Via::Cpxy, proxy, "{input}");
            assert_eq!(matches!(spec.endpoint, Endpoint::Tcp(_)), tcp, "{input}");
        }
    }

    #[tokio::test]
    async fn overseas_dns_reaches_socks5_as_tcp_with_original_address() {
        for destination in ["8.8.8.8:53", "[2001:4860:4860::8888]:53"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = listener.local_addr().unwrap();
            let expected: SocketAddr = destination.parse().unwrap();
            let fixture = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let (request, handshake) = SocksProxyHandshaker::accept(socket).await.unwrap();
                assert!(matches!(request, ProxyRequest::WithIP(addr) if addr == expected));
                let mut stream = handshake.respond_ok().await.unwrap();
                let length = stream.read_u16().await.unwrap();
                let mut bytes = vec![0; length as usize];
                stream.read_exact(&mut bytes).await.unwrap();
                let mut response = Message::from_vec(&bytes).unwrap();
                assert_eq!(response.queries()[0].name().to_ascii(), "example.com.");
                response.set_message_type(MessageType::Response);
                let bytes = response.to_vec().unwrap();
                stream.write_u16(bytes.len() as u16).await.unwrap();
                stream.write_all(&bytes).await.unwrap();
            });
            let spec = router_spec(&destination.parse().unwrap(), &ProxyRoutes::default())
                .await
                .unwrap();
            let upstream = HickoryUpstream::new(
                &spec,
                tls_client_config().unwrap(),
                QUERY_TIMEOUT,
                Some(Arc::new(OutboundDialer(Arc::new(Socks5Outbound(proxy))))),
            )
            .await
            .unwrap();
            let mut query = Message::new();
            query.add_query(Query::query(
                Name::from_ascii("example.com.").unwrap(),
                RecordType::A,
            ));
            let answer = upstream.query(query).await.unwrap();
            assert_eq!(answer.message_type(), MessageType::Response);
            fixture.await.unwrap();
        }
    }
}
