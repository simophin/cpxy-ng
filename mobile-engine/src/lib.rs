//! The packet engine of the mobile VPN app: reads IP packets from the platform's TUN device and
//! does what `cpxy-router` does on OpenWrt (see `docs/mobile-vpn-plan.md`):
//!
//! - DNS sent to [`DNS_ADDR`] is answered with `dns_split`. As a Tailscale exit node
//!   ([`start_gateway`]), DNS to any address is answered this way. A router ([`start_router`])
//!   answers it on its own sockets instead.
//! - TCP to private and CN addresses goes direct, everything else through the cpxy server. When
//!   the server is unreachable the connection fails; it is never sent direct instead.
//! - UDP 443 (QUIC) and TCP 853 (DNS over TLS) are refused; other UDP is relayed direct.
//! - IPv6 is handled like IPv4, or refused, according to the config's [`Ipv6Mode`].
//!
//! The engine's own sockets must bypass the tunnel; the platform arranges that.

#[cfg(target_os = "linux")]
pub mod capi;
mod config;
mod connection_log;
mod dns;
mod ffi;
mod filter;
mod fin;
#[cfg(target_os = "linux")]
pub mod linux;
mod proxy;
mod routing;
mod sni;
mod tun;

pub use client::stats_server::OutboundEvent;
pub use config::{Config, DEFAULT_MTU, DNS_ADDR, TUN_ADDR, TUN_PREFIX_LEN};
pub use connection_log::{ConnectionPage, ConnectionRecord};
pub use ffi::{ConnectionEvent, Engine, EngineError, start_engine};

uniffi::setup_scaffolding!();

use client::dns_split::server::DnsSplitHandler;
use client::dns_split::spec::ServerSpec;
pub use client::ipv6::Ipv6Mode;
use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
use hickory_proto::op::Message;
use hickory_proto::rr::Name;
use ipstack::{
    IpStack, IpStackConfig, IpStackStream, IpStackTcpStream, IpStackUdpStream, TcpConfig,
    TcpOptions,
};
use routing::Router;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;
use tokio::sync::{Notify, broadcast, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tun::{Traffic, TunDevice};

/// Small, to stay within the iOS packet tunnel extension's memory limit. ipstack needs a
/// multi-threaded runtime: its TCP streams block in place when dropped.
const WORKER_THREADS: usize = 2;
/// Idle TCP flows are closed after this long. Longer than ipstack's default of a minute, which
/// would cut idle connections such as push notification channels.
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(3600);
/// Idle UDP flows are closed, and their direct sockets released, after this long.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const DNS_PORT: u16 = 53;
/// IPv6 and TCP headers without options. The MSS counts payload only (RFC 6691), and ipstack
/// advertises one MSS for both IP versions, so it allows for the larger IPv6 header.
const TCP_IP_HEADERS_LEN: u16 = 40 + 20;
/// The most a TCP flow has in flight towards the device, and buffers from it before the outbound
/// side reads it, which is also the window it advertises; a flow moves at most this much per
/// round trip between the device's apps and the engine. ipstack's default is 16 KiB.
///
/// On a phone or a router the apps are a millisecond away, and memory is tight.
const LOCAL_TCP_WINDOW: u32 = 64 << 10;
/// An exit node's clients are a network away: 1 MiB is 50 MB/s at 20 ms, or 10 MB/s at 100 ms.
const GATEWAY_TCP_WINDOW: u32 = 1 << 20;
/// How long stopping waits for the flows to end, and then for the runtime to shut down.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a connection waits for the app to name its server before it is reported under its
/// address. Only the report waits: apps that send first have sent by now, and the rest, such as
/// SSH, wait for the server.
const SNI_WAIT: Duration = Duration::from_secs(1);

/// Receives what the engine reports while it runs. Called from engine threads.
pub trait EventListener: Send + Sync {
    /// A TCP flow was connected direct or through the proxy, or failed to connect.
    fn on_outbound_event(&self, event: OutboundEvent);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, uniffi::Record)]
pub struct TrafficStats {
    /// IP bytes from the device into the tunnel.
    pub sent: u64,
    /// IP bytes from the tunnel to the device.
    pub received: u64,
}

pub struct EngineHandle {
    running: Mutex<Option<Running>>,
    traffic: Arc<Traffic>,
    /// Answers [`EngineHandle::resolve_dns`]; `None` when the engine leaves DNS alone.
    dns: Option<Arc<DnsSplitHandler>>,
    local_proxy_port: Option<u16>,
}

struct Running {
    runtime: Runtime,
    shutdown: oneshot::Sender<()>,
    /// Ends once every flow has ended and the TUN descriptor is closed.
    stopped: JoinHandle<()>,
}

/// Starts the engine on a TUN device the platform has set up with [`TUN_ADDR`], [`DNS_ADDR`] as
/// its DNS server, and the config's MTU. Blocks while the DNS servers are set up, which may look
/// up their hostnames. The engine owns the descriptor and closes it when stopped.
///
/// The app reaches the network the way the VPN does through
/// [`EngineHandle::local_proxy_port`].
pub fn start(
    tun: OwnedFd,
    config_json: &str,
    listener: Arc<dyn EventListener>,
) -> anyhow::Result<EngineHandle> {
    start_with_dns(
        tun,
        config_json,
        listener,
        DnsIntercept::VirtualServer,
        LOCAL_TCP_WINDOW,
        true,
    )
}

/// Starts the engine for a Tailscale exit node (`cpxy-tsgw`), where the device is the exit
/// node's side of WireGuard rather than a TUN the platform set up. Like [`start`], except that DNS
/// to any address is answered: the exit node's clients send DNS to whatever servers they have.
pub fn start_gateway(
    tun: OwnedFd,
    config_json: &str,
    listener: Arc<dyn EventListener>,
) -> anyhow::Result<EngineHandle> {
    start_with_dns(
        tun,
        config_json,
        listener,
        DnsIntercept::AnyServer,
        GATEWAY_TCP_WINDOW,
        false,
    )
}

fn start_with_dns(
    tun: OwnedFd,
    config_json: &str,
    listener: Arc<dyn EventListener>,
    intercept: DnsIntercept,
    tcp_window: u32,
    local_proxy: bool,
) -> anyhow::Result<EngineHandle> {
    let settings = Config::from_json(config_json)?.settings()?;
    start_inner(
        tun,
        settings.server,
        settings.proxy_routes,
        settings.ipv6,
        settings.mtu,
        tcp_window,
        listener,
        proxy::Ports {
            http: settings.http_proxy_port,
            socks: settings.socks_proxy_port,
            local_http: local_proxy,
        },
        Some(DnsSpecs {
            split: dns::Specs {
                upstream: &settings.dns_upstream,
                alternative: &settings.dns_alternative,
                search_domains: &settings.dns_search_domains,
                local: None,
                cache: true,
                ipv6: settings.ipv6,
            },
            intercept: Some(intercept),
            listen: None,
        }),
    )
}

/// The DNS a router answers on its own sockets, where its firewall redirects its networks' DNS.
/// It never reaches the TUN.
pub struct RouterDns {
    pub listen: SocketAddr,
    pub upstream: Vec<ServerSpec>,
    pub alternative: Vec<ServerSpec>,
    pub search_domains: Vec<Name>,
    /// The router's own resolver (dnsmasq), for single-label names, names under
    /// `local_domains` and reverse lookups of private addresses.
    pub local_server: Option<ServerSpec>,
    pub local_domains: Vec<Name>,
    pub cache: bool,
}

/// Starts the shared packet engine for a router, which answers DNS on `dns.listen` rather than
/// from the TUN. No SOCKS listener is needed.
pub fn start_router(
    tun: OwnedFd,
    server: client::protocol_config::Config,
    proxy_routes: client::outbound::ProxyRoutes,
    ipv6: Ipv6Mode,
    mtu: u16,
    dns: Option<RouterDns>,
    listener: Arc<dyn EventListener>,
) -> anyhow::Result<EngineHandle> {
    let dns_specs = dns.as_ref().map(|dns| DnsSpecs {
        split: dns::Specs {
            upstream: &dns.upstream,
            alternative: &dns.alternative,
            search_domains: &dns.search_domains,
            local: dns
                .local_server
                .as_ref()
                .map(|server| (server, dns.local_domains.as_slice())),
            cache: dns.cache,
            ipv6,
        },
        intercept: None,
        listen: Some(dns.listen),
    });
    start_inner(
        tun,
        server,
        proxy_routes,
        ipv6,
        mtu,
        LOCAL_TCP_WINDOW,
        listener,
        proxy::Ports::default(),
        dns_specs,
    )
}

/// Which DNS packets the engine answers itself rather than relays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DnsIntercept {
    /// Only DNS to [`DNS_ADDR`], the virtual server the platform hands to the device.
    VirtualServer,
    /// DNS to any address.
    AnyServer,
}

impl DnsIntercept {
    fn matches(self, dst: SocketAddr) -> bool {
        dst.port() == DNS_PORT
            && match self {
                Self::VirtualServer => dst.ip() == DNS_ADDR,
                Self::AnyServer => true,
            }
    }
}

struct DnsSpecs<'a> {
    split: dns::Specs<'a>,
    /// The DNS from the TUN that is answered; `None` relays it all.
    intercept: Option<DnsIntercept>,
    /// Where to answer DNS on the engine's own sockets.
    listen: Option<SocketAddr>,
}

/// The DNS the engine answers, and how.
#[derive(Clone)]
struct DnsService {
    handler: Arc<DnsSplitHandler>,
    intercept: Option<DnsIntercept>,
}

impl DnsService {
    /// The handler to answer a flow to `dst` with, if the engine answers it.
    fn handler_for(dns: Option<Self>, dst: SocketAddr) -> Option<Arc<DnsSplitHandler>> {
        dns.filter(|dns| dns.intercept.is_some_and(|i| i.matches(dst)))
            .map(|dns| dns.handler)
    }
}

fn start_inner(
    tun: OwnedFd,
    server: client::protocol_config::Config,
    proxy_routes: client::outbound::ProxyRoutes,
    ipv6: Ipv6Mode,
    mtu: u16,
    tcp_window: u32,
    listener: Arc<dyn EventListener>,
    proxy_ports: proxy::Ports,
    dns_specs: Option<DnsSpecs>,
) -> anyhow::Result<EngineHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("cpxy-engine")
        .enable_all()
        .build()?;
    let dns = dns_specs
        .as_ref()
        .map(|specs| {
            runtime
                .block_on(dns::handler(&specs.split, &server))
                .map(|handler| DnsService {
                    handler: Arc::new(handler),
                    intercept: specs.intercept,
                })
        })
        .transpose()?;
    // Bind before reporting success, so an occupied port fails the connection visibly.
    let proxies = runtime.block_on(proxy::Listeners::bind(proxy_ports))?;
    let dns_listeners = dns_specs
        .and_then(|specs| specs.listen)
        .map(|addr| runtime.block_on(dns::Listeners::bind(addr)))
        .transpose()?;
    let local_proxy_port = proxies.local_http_port();
    let traffic = Arc::new(Traffic::default());
    let (device_closed_tx, device_closed) = oneshot::channel();
    let device_eof = Arc::new(Notify::new());
    let device = {
        let _guard = runtime.enter();
        TunDevice::new(
            tun,
            mtu,
            filter::Filter::new(ipv6, proxy_routes.clone()),
            traffic.clone(),
            device_eof.clone(),
            device_closed_tx,
        )?
    };

    let (events_tx, events_rx) = broadcast::channel(256);
    let router = Arc::new(Router::new(server, proxy_routes, events_tx));
    let (shutdown, shutdown_rx) = oneshot::channel();
    runtime.spawn(forward_events(events_rx, listener));
    let resolver = dns.as_ref().map(|dns| dns.handler.clone());
    let stopped = runtime.spawn(run(
        device,
        device_closed,
        device_eof,
        mtu,
        tcp_window,
        router,
        dns,
        proxies,
        dns_listeners,
        shutdown_rx,
    ));
    tracing::info!("Engine started");

    Ok(EngineHandle {
        running: Mutex::new(Some(Running {
            runtime,
            shutdown,
            stopped,
        })),
        traffic,
        dns: resolver,
        local_proxy_port,
    })
}

impl EngineHandle {
    /// Stops the engine and closes the TUN descriptor. Must not be called from an engine thread,
    /// such as an [`EventListener`] callback.
    pub fn stop(&self) {
        let Some(running) = self.running.lock().unwrap().take() else {
            return;
        };
        let _ = running.shutdown.send(());
        // Wait while the runtime still works: dropping a flow waits for its tasks.
        let stopped = running
            .runtime
            .block_on(async { tokio::time::timeout(STOP_TIMEOUT, running.stopped).await });
        if stopped.is_err() {
            tracing::warn!("Engine did not stop in time; the TUN descriptor may stay open");
        }
        running.runtime.shutdown_timeout(STOP_TIMEOUT);
        tracing::info!("Engine stopped");
    }

    /// False when the packet worker has ended, so a supervisor can restart it.
    pub fn is_running(&self) -> bool {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| !r.stopped.is_finished())
    }

    /// The loopback port of an HTTP proxy that connects the way the VPN does, for the app's own
    /// requests, which bypass the VPN. Only [`start`] opens one.
    pub fn local_proxy_port(&self) -> Option<u16> {
        self.local_proxy_port
    }

    pub fn traffic(&self) -> TrafficStats {
        TrafficStats {
            sent: self.traffic.sent.load(Ordering::Relaxed),
            received: self.traffic.received.load(Ordering::Relaxed),
        }
    }

    /// Answers one wire-format DNS query the way the engine answers DNS it intercepts, for DNS
    /// that reaches the platform some other way (a Tailscale exit node's DNS over the peer API).
    /// Blocks the calling thread, which must not be an engine thread. May be called from several
    /// threads at once, but not while the engine is being stopped.
    pub fn resolve_dns(&self, query: &[u8]) -> anyhow::Result<Vec<u8>> {
        let handler = self
            .dns
            .clone()
            .ok_or_else(|| anyhow::anyhow!("The engine does not answer DNS"))?;
        // Not held while resolving, so queries run concurrently.
        let runtime = self
            .running
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.runtime.handle().clone())
            .ok_or_else(|| anyhow::anyhow!("The engine is stopped"))?;
        let request = Message::from_vec(query)?;
        let response = runtime.block_on(handler.handle(&request));
        Ok(response.to_vec()?)
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn forward_events(
    mut events: broadcast::Receiver<OutboundEvent>,
    listener: Arc<dyn EventListener>,
) {
    loop {
        match events.recv().await {
            Ok(event) => listener.on_outbound_event(event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!("Listener fell behind, skipped {n} events")
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn run(
    device: TunDevice,
    device_closed: oneshot::Receiver<()>,
    device_eof: Arc<Notify>,
    mtu: u16,
    tcp_window: u32,
    router: Arc<Router>,
    dns: Option<DnsService>,
    proxies: proxy::Listeners,
    dns_listeners: Option<dns::Listeners>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut tcp_config = TcpConfig::default();
    tcp_config.timeout = TCP_IDLE_TIMEOUT;
    tcp_config.max_unacked_bytes = tcp_window;
    tcp_config.read_buffer_size = tcp_window as usize;
    // Without an MSS in the SYN-ACK, peers fall back to 536-byte segments (RFC 1122)
    tcp_config.options = Some(vec![TcpOptions::MaximumSegmentSize(mss_for_mtu(mtu))]);
    let mut config = IpStackConfig::default();
    config
        .mtu_unchecked(mtu)
        .udp_timeout(UDP_IDLE_TIMEOUT)
        .with_tcp_config(tcp_config);
    let mut stack = IpStack::new(config, device);
    let mut listener_tasks =
        proxies.spawn(router.clone(), dns.as_ref().map(|dns| dns.handler.clone()));
    if let (Some(listeners), Some(dns)) = (dns_listeners, &dns) {
        listeners.spawn(dns.handler.clone(), &mut listener_tasks);
    }
    let mut flows = JoinSet::new();

    loop {
        let stream = tokio::select! {
            stream = stack.accept() => stream,
            // Reap the flows that ended.
            Some(_) = flows.join_next() => continue,
            Some(result) = listener_tasks.join_next() => {
                tracing::error!(?result, "Proxy or DNS listener stopped");
                break;
            }
            _ = &mut shutdown => break,
            _ = device_eof.notified() => {
                tracing::warn!("The device closed");
                break;
            }
        };
        match stream {
            Ok(IpStackStream::Tcp(tcp)) => {
                flows.spawn(handle_tcp(tcp, router.clone(), dns.clone()));
            }
            Ok(IpStackStream::Udp(udp)) => {
                flows.spawn(handle_udp(udp, dns.clone()));
            }
            // ICMP and anything else the filter let through: not relayed.
            Ok(IpStackStream::UnknownTransport(_) | IpStackStream::UnknownNetwork(_)) => {}
            Err(e) => {
                tracing::error!("IP stack stopped: {e}");
                break;
            }
        }
    }

    // Stop in order. ipstack's TCP streams wait for their tasks when dropped, which never happens
    // once the runtime shuts down, so the flows must end first. Dropping the stack aborts the task
    // that owns the device.
    drop(stack);
    listener_tasks.shutdown().await;
    flows.shutdown().await;
    let _ = device_closed.await;
}

fn mss_for_mtu(mtu: u16) -> u16 {
    mtu.saturating_sub(TCP_IP_HEADERS_LEN)
}

/// The connection is reported under its server name once the app's ClientHello names it, or
/// under its address when no name comes within [`SNI_WAIT`].
async fn handle_tcp(flow: IpStackTcpStream, router: Arc<Router>, dns: Option<DnsService>) {
    let dst = flow.peer_addr();
    if let Some(handler) = DnsService::handler_for(dns, dst) {
        return dns::serve_tcp_flow(flow, handler).await;
    }

    let address = dst.ip().to_string();
    let request = OutboundRequest {
        host: OutboundHost::Resolved {
            domain: address.clone(),
            ip: Some(dst.ip()),
        },
        port: dst.port(),
        tls: false,
        initial_plaintext: Vec::new(),
    };
    // The app's first bytes wait in the flow while connecting, and the tap reads the server name
    // from them as they are relayed, so connecting never waits for it.
    let (name_tx, mut name_rx) = oneshot::channel();
    let mut flow = sni::SniTap::new(fin::FinRetry::new(flow), name_tx);
    let (outcome, upstream) = router.connect(request).await;
    let mut upstream = match upstream {
        Ok(upstream) => upstream,
        Err(_) => {
            flow.sniff_buffered().await;
            let name = name_rx.try_recv().ok().flatten();
            router.report(outcome, name.unwrap_or(address));
            // Dropping the flow resets the app's connection.
            return;
        }
    };

    let relay = async move {
        if let Err(e) = tokio::io::copy_bidirectional(&mut flow, &mut upstream).await {
            tracing::debug!("TCP flow to {dst} ended: {e}");
        }
        // Dropping the tap here ends the wait for a name.
    };
    let report = async {
        let name = tokio::time::timeout(SNI_WAIT, name_rx).await;
        let name = name.ok().and_then(Result::ok).flatten();
        router.report(outcome, name.unwrap_or(address));
    };
    tokio::join!(relay, report);
}

async fn handle_udp(flow: IpStackUdpStream, dns: Option<DnsService>) {
    let dst = flow.peer_addr();
    if let Some(handler) = DnsService::handler_for(dns, dst) {
        return dns::serve_udp_flow(flow, handler).await;
    }
    if let Err(e) = relay_udp(flow, dst).await {
        tracing::debug!("UDP flow to {dst} ended: {e}");
    }
}

/// Relays one UDP flow through its own direct socket until ipstack closes it as idle.
async fn relay_udp(mut flow: IpStackUdpStream, dst: SocketAddr) -> std::io::Result<()> {
    let unspecified: IpAddr = match dst {
        SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let socket = UdpSocket::bind((unspecified, 0)).await?;
    socket.connect(dst).await?;

    let mut from_app = vec![0u8; 65535];
    let mut from_remote = vec![0u8; 65535];
    loop {
        tokio::select! {
            n = flow.read(&mut from_app) => {
                match n? {
                    0 => return Ok(()),
                    n => { socket.send(&from_app[..n]).await?; }
                }
            }
            n = socket.recv(&mut from_remote) => {
                flow.write_all(&from_remote[..n?]).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};

    struct Ignore;
    impl EventListener for Ignore {
        fn on_outbound_event(&self, _: OutboundEvent) {}
    }

    fn is_open(fd: i32) -> bool {
        // SAFETY: F_GETFD only inspects the descriptor table.
        unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    }

    #[test]
    fn stops_when_the_other_end_of_a_socket_closes() {
        let mut fds = [0; 2];
        // SAFETY: socketpair fills in two descriptors we then own.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr()) },
            0
        );
        // SAFETY: both descriptors were just created and are owned here.
        let (tun, peer) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let config = r#"{"server": "http://:k@127.0.0.1:1", "dns_upstream": ["127.0.0.1"], "dns_alternative": ["127.0.0.1"]}"#;
        let engine = start_gateway(tun, config, Arc::new(Ignore)).unwrap();
        assert!(engine.is_running());

        drop(peer);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.is_running() {
            assert!(
                std::time::Instant::now() < deadline,
                "the engine still runs"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn dns_interception_depends_on_the_mode() {
        let virtual_server = SocketAddr::from((DNS_ADDR, 53));
        let public = SocketAddr::from((Ipv4Addr::new(8, 8, 8, 8), 53));
        let other_port = SocketAddr::from((Ipv4Addr::new(8, 8, 8, 8), 5353));
        let public_v6: SocketAddr = "[2001:4860:4860::8888]:53".parse().unwrap();

        assert!(DnsIntercept::VirtualServer.matches(virtual_server));
        assert!(!DnsIntercept::VirtualServer.matches(public));
        assert!(DnsIntercept::AnyServer.matches(virtual_server));
        assert!(DnsIntercept::AnyServer.matches(public));
        assert!(!DnsIntercept::AnyServer.matches(other_port));
        assert!(!DnsIntercept::VirtualServer.matches(public_v6));
        assert!(DnsIntercept::AnyServer.matches(public_v6));
        assert!(!DnsIntercept::VirtualServer.matches(SocketAddr::from((DNS_ADDR, 80))));
    }

    #[test]
    fn mss_fits_an_ipv6_packet_in_the_mtu() {
        assert_eq!(mss_for_mtu(1280), 1220);
        assert_eq!(mss_for_mtu(1500), 1440);
        assert_eq!(mss_for_mtu(40), 0);
    }

    #[test]
    fn stopping_engine_closes_proxy_listeners_and_pending_handshakes() {
        use std::io::{Read, Write};
        let reserved = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let mut fds = [0; 2];
        // SAFETY: socketpair fills in two new descriptors, owned below.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) },
            0
        );
        let (tun, _peer) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let config = format!(
            r#"{{"server":"http://:k@127.0.0.1:1", "dns_upstream":["127.0.0.1"], "dns_alternative":["127.0.0.1"], "socks_proxy_port":{port}}}"#
        );
        let engine = start(tun, &config, Arc::new(Ignore)).unwrap();
        let mut client = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(&[5, 1, 0]).unwrap();
        let mut reply = [0; 2];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [5, 0]);
        engine.stop();
        assert_eq!(client.read(&mut reply).unwrap(), 0);
        assert!(std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
        // A subsequent connection can bind the saved port again.
        std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap();
    }

    #[test]
    fn router_answers_dns_on_its_listen_address_until_stopped() {
        use hickory_proto::op::Query;
        use hickory_proto::rr::RecordType;
        use std::io::{Read, Write};
        use std::os::unix::net::UnixDatagram;

        let reserved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let listen = reserved.local_addr().unwrap();
        drop(reserved);
        let (tun, _peer) = UnixDatagram::pair().unwrap();
        let spec = |s: &str| s.parse::<ServerSpec>().unwrap();
        let engine = start_router(
            tun.into(),
            "http://:k@127.0.0.1:1".parse().unwrap(),
            client::outbound::ProxyRoutes(Vec::new()),
            Ipv6Mode::Full,
            DEFAULT_MTU,
            Some(RouterDns {
                listen,
                upstream: vec![spec("127.0.0.1")],
                alternative: vec![spec("127.0.0.1")],
                search_domains: Vec::new(),
                local_server: Some(spec("127.0.0.1")),
                local_domains: vec![Name::from_ascii("lan").unwrap()],
                cache: true,
            }),
            Arc::new(Ignore),
        )
        .unwrap();

        // AAAA is answered without asking any server.
        let mut query = Message::new();
        query.set_id(7).add_query(Query::query(
            Name::from_ascii("example.com.").unwrap(),
            RecordType::AAAA,
        ));
        let query = query.to_vec().unwrap();

        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        udp.send_to(&query, listen).unwrap();
        let mut buf = [0; 512];
        let n = udp.recv(&mut buf).unwrap();
        assert_eq!(Message::from_vec(&buf[..n]).unwrap().id(), 7);

        let mut tcp = std::net::TcpStream::connect(listen).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tcp.write_all(&(query.len() as u16).to_be_bytes()).unwrap();
        tcp.write_all(&query).unwrap();
        let mut len = [0; 2];
        tcp.read_exact(&mut len).unwrap();
        let mut reply = vec![0; u16::from_be_bytes(len).into()];
        tcp.read_exact(&mut reply).unwrap();
        assert_eq!(Message::from_vec(&reply).unwrap().id(), 7);

        engine.stop();
        std::net::UdpSocket::bind(listen).unwrap();
        std::net::TcpListener::bind(listen).unwrap();
    }

    #[test]
    fn reports_a_tls_connection_under_its_server_name() {
        use std::io::Read;
        use std::os::unix::net::UnixDatagram;

        struct Channel(Mutex<std::sync::mpsc::Sender<OutboundEvent>>);
        impl EventListener for Channel {
            fn on_outbound_event(&self, event: OutboundEvent) {
                let _ = self.0.lock().unwrap().send(event);
            }
        }

        let (tun, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (events_tx, events) = std::sync::mpsc::channel();
        let config = r#"{"server": "http://:k@127.0.0.1:1", "dns_upstream": ["127.0.0.1"], "dns_alternative": ["127.0.0.1"]}"#;
        let engine = start(tun.into(), config, Arc::new(Channel(Mutex::new(events_tx)))).unwrap();

        // Loopback goes direct, to this listener.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let send = |seq: u32, ack: Option<u32>, payload: &[u8]| {
            let builder = etherparse::PacketBuilder::ipv4(TUN_ADDR.octets(), [127, 0, 0, 1], 64)
                .tcp(40000, port, seq, 65535);
            let builder = match ack {
                Some(ack) => builder.ack(ack).psh(),
                None => builder.syn(),
            };
            let mut packet = Vec::with_capacity(builder.size(payload.len()));
            builder.write(&mut packet, payload).unwrap();
            peer.send(&packet).unwrap();
        };
        send(1, None, &[]);
        let mut packet = [0u8; 2048];
        let syn_ack = loop {
            let n = peer.recv(&mut packet).unwrap();
            let sliced = etherparse::SlicedPacket::from_ip(&packet[..n]).unwrap();
            if let Some(etherparse::TransportSlice::Tcp(tcp)) = sliced.transport
                && tcp.syn()
                && tcp.ack()
            {
                break tcp.sequence_number();
            }
        };
        let hello = sni::tests::client_hello(Some(&sni::tests::server_name_ext(b"example.com")));
        send(2, Some(syn_ack.wrapping_add(1)), &hello);

        let (mut accepted, _) = listener.accept().unwrap();
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut relayed = vec![0u8; hello.len()];
        accepted.read_exact(&mut relayed).unwrap();
        assert_eq!(relayed, hello);

        match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            OutboundEvent::Connected {
                host, ip, outbound, ..
            } => {
                assert_eq!(host, "example.com");
                assert_eq!(ip, Some(Ipv4Addr::LOCALHOST.into()));
                assert_eq!(outbound, "direct");
            }
            OutboundEvent::Error { error, .. } => panic!("connecting failed: {error}"),
        }
        engine.stop();
    }

    #[test]
    fn relays_ipv6_unless_it_is_off() {
        use std::io::Read;
        use std::os::unix::net::UnixDatagram;

        const APP: Ipv6Addr = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
        let config = |mode: &str| {
            format!(
                r#"{{"server": "http://:k@127.0.0.1:1", "dns_upstream": ["127.0.0.1"], "dns_alternative": ["127.0.0.1"], "ipv6": "{mode}"}}"#
            )
        };
        let packet_to = |port: u16, tcp: Option<(u32, Option<u32>)>, payload: &[u8]| {
            let builder =
                etherparse::PacketBuilder::ipv6(APP.octets(), Ipv6Addr::LOCALHOST.octets(), 64);
            let mut packet = Vec::new();
            match tcp {
                Some((seq, None)) => builder
                    .tcp(40000, port, seq, 65535)
                    .syn()
                    .write(&mut packet, payload),
                Some((seq, Some(ack))) => builder
                    .tcp(40000, port, seq, 65535)
                    .ack(ack)
                    .psh()
                    .write(&mut packet, payload),
                None => builder.udp(40000, port).write(&mut packet, payload),
            }
            .unwrap();
            packet
        };

        // Loopback goes direct, to these sockets.
        let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
        let tcp_port = listener.local_addr().unwrap().port();
        let echo = std::net::UdpSocket::bind("[::1]:0").unwrap();
        echo.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let udp_port = echo.local_addr().unwrap().port();

        let (tun, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let engine = start(tun.into(), &config("direct"), Arc::new(Ignore)).unwrap();
        let mut buf = [0u8; 2048];

        peer.send(&packet_to(tcp_port, Some((1, None)), &[]))
            .unwrap();
        let syn_ack = loop {
            let n = peer.recv(&mut buf).unwrap();
            let sliced = etherparse::SlicedPacket::from_ip(&buf[..n]).unwrap();
            if let Some(etherparse::TransportSlice::Tcp(tcp)) = sliced.transport
                && tcp.syn()
                && tcp.ack()
            {
                break tcp.sequence_number();
            }
        };
        peer.send(&packet_to(
            tcp_port,
            Some((2, Some(syn_ack.wrapping_add(1)))),
            b"hello",
        ))
        .unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        accepted
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut relayed = [0u8; 5];
        accepted.read_exact(&mut relayed).unwrap();
        assert_eq!(&relayed, b"hello");

        peer.send(&packet_to(udp_port, None, b"ping")).unwrap();
        let (n, from) = echo.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
        assert!(from.is_ipv6());
        engine.stop();

        let (tun, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let engine = start(tun.into(), &config("off"), Arc::new(Ignore)).unwrap();
        peer.send(&packet_to(tcp_port, Some((1, None)), &[]))
            .unwrap();
        let n = peer.recv(&mut buf).unwrap();
        let sliced = etherparse::SlicedPacket::from_ip(&buf[..n]).unwrap();
        assert!(matches!(
            sliced.transport,
            Some(etherparse::TransportSlice::Icmpv6(_))
        ));
        engine.stop();
    }

    #[test]
    fn stop_closes_the_tun_descriptor() {
        let mut fds = [0; 2];
        // SAFETY: socketpair fills in two descriptors we then own.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) },
            0
        );
        // SAFETY: both descriptors were just created and are owned here.
        let (tun, _peer) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let raw = tun.as_raw_fd();

        let config = r#"{"server": "http://:k@127.0.0.1:1", "dns_upstream": ["127.0.0.1"], "dns_alternative": ["127.0.0.1"]}"#;
        let engine = start(tun, config, Arc::new(Ignore)).unwrap();
        assert!(is_open(raw));

        // Keep a TCP flow open while stopping: loopback goes direct, to this listener.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let syn = {
            let builder = etherparse::PacketBuilder::ipv4(TUN_ADDR.octets(), [127, 0, 0, 1], 64)
                .tcp(40000, port, 1, 65535)
                .syn();
            let mut packet = Vec::with_capacity(builder.size(0));
            builder.write(&mut packet, &[]).unwrap();
            packet
        };
        // SAFETY: writing our own buffer to the peer socket.
        let sent = unsafe { libc::write(_peer.as_raw_fd(), syn.as_ptr() as *const _, syn.len()) };
        assert_eq!(sent, syn.len() as isize);
        let _accepted = listener.accept().unwrap();

        engine.stop();
        // Checked through the peer, not the descriptor number, which other tests may reuse at once.
        // SAFETY: writing our own buffer to the peer socket.
        let sent = unsafe { libc::write(_peer.as_raw_fd(), syn.as_ptr() as *const _, syn.len()) };
        assert!(sent < 0, "the TUN descriptor is still open");
    }
}
