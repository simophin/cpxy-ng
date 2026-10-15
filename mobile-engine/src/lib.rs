//! The packet engine of the mobile VPN app: reads IP packets from the platform's TUN device and
//! does what `cpxy-router` does on OpenWrt (see `docs/mobile-vpn-plan.md`):
//!
//! - DNS sent to [`DNS_ADDR`] is answered with `dns_split`. With `block_ads`, names on the
//!   built-in blocklist resolve to [`config::SINKHOLE_ADDR`], and packets to the sinkhole range
//!   are dropped without a reply. As a Tailscale exit node ([`start_gateway`]), DNS to any
//!   address is answered this way.
//! - TCP to private and CN addresses goes direct, everything else through the cpxy server. When
//!   the server is unreachable the connection fails; it is never sent direct instead.
//! - UDP 443 (QUIC) and TCP 853 (DNS over TLS) are refused; other UDP is relayed direct.
//! - IPv6 is refused.
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
use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
use hickory_proto::op::Message;
use ipstack::{
    IpStack, IpStackConfig, IpStackStream, IpStackTcpStream, IpStackUdpStream, TcpConfig,
    TcpOptions,
};
use routing::Router;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
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
    )
}

fn start_with_dns(
    tun: OwnedFd,
    config_json: &str,
    listener: Arc<dyn EventListener>,
    intercept: DnsIntercept,
    tcp_window: u32,
) -> anyhow::Result<EngineHandle> {
    let settings = Config::from_json(config_json)?.settings()?;
    start_inner(
        tun,
        settings.server,
        settings.proxy_routes,
        settings.mtu,
        tcp_window,
        listener,
        proxy::Ports {
            http: settings.http_proxy_port,
            socks: settings.socks_proxy_port,
        },
        Some(DnsSpecs {
            upstream: &settings.dns_upstream,
            alternative: &settings.dns_alternative,
            search_domains: &settings.dns_search_domains,
            // Only referenced here, so cpxy-router does not embed the list.
            blocklist: settings.block_ads.then_some(&blocklist_data::BASELINE),
            intercept,
        }),
    )
}

/// Starts the shared packet engine for a router. DNS stays with dnsmasq/dns_split;
/// no synthetic DNS address is intercepted and no SOCKS listener is needed.
pub fn start_router(
    tun: OwnedFd,
    server: client::protocol_config::Config,
    proxy_routes: client::outbound::ProxyRoutes,
    mtu: u16,
    listener: Arc<dyn EventListener>,
) -> anyhow::Result<EngineHandle> {
    start_inner(
        tun,
        server,
        proxy_routes,
        mtu,
        LOCAL_TCP_WINDOW,
        listener,
        proxy::Ports::default(),
        None,
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
    fn matches(self, dst: SocketAddrV4) -> bool {
        dst.port() == DNS_PORT
            && match self {
                Self::VirtualServer => *dst.ip() == DNS_ADDR,
                Self::AnyServer => true,
            }
    }
}

struct DnsSpecs<'a> {
    upstream: &'a [client::dns_split::spec::ServerSpec],
    alternative: &'a [client::dns_split::spec::ServerSpec],
    search_domains: &'a [hickory_proto::rr::Name],
    blocklist: Option<&'static blocklist_data::Blocklist>,
    intercept: DnsIntercept,
}

/// The DNS the engine answers, and how.
#[derive(Clone)]
struct DnsService {
    handler: Arc<DnsSplitHandler>,
    intercept: DnsIntercept,
}

impl DnsService {
    /// The handler to answer a flow to `dst` with, if the engine answers it.
    fn handler_for(dns: Option<Self>, dst: SocketAddrV4) -> Option<Arc<DnsSplitHandler>> {
        dns.filter(|dns| dns.intercept.matches(dst))
            .map(|dns| dns.handler)
    }
}

fn start_inner(
    tun: OwnedFd,
    server: client::protocol_config::Config,
    proxy_routes: client::outbound::ProxyRoutes,
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
        .map(|specs| {
            runtime
                .block_on(dns::handler(
                    specs.upstream,
                    specs.alternative,
                    specs.search_domains,
                    &server,
                    specs.blocklist,
                ))
                .map(|handler| DnsService {
                    handler: Arc::new(handler),
                    intercept: specs.intercept,
                })
        })
        .transpose()?;
    // Bind before reporting success, so an occupied port fails the connection visibly.
    let proxies = runtime.block_on(proxy::Listeners::bind(proxy_ports))?;
    let traffic = Arc::new(Traffic::default());
    let (device_closed_tx, device_closed) = oneshot::channel();
    let device_eof = Arc::new(Notify::new());
    let device = {
        let _guard = runtime.enter();
        TunDevice::new(
            tun,
            mtu,
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
    let mut proxy_tasks =
        proxies.spawn(router.clone(), dns.as_ref().map(|dns| dns.handler.clone()));
    let mut flows = JoinSet::new();

    loop {
        let stream = tokio::select! {
            stream = stack.accept() => stream,
            // Reap the flows that ended.
            Some(_) = flows.join_next() => continue,
            Some(result) = proxy_tasks.join_next() => {
                tracing::error!(?result, "Proxy listener stopped");
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
    proxy_tasks.shutdown().await;
    flows.shutdown().await;
    let _ = device_closed.await;
}

fn mss_for_mtu(mtu: u16) -> u16 {
    mtu.saturating_sub(TCP_IP_HEADERS_LEN)
}

fn ipv4_dst(addr: SocketAddr) -> Option<SocketAddrV4> {
    match addr {
        SocketAddr::V4(addr) => Some(addr),
        // The filter refuses IPv6 before it reaches the stack.
        SocketAddr::V6(_) => None,
    }
}

/// The connection is reported under its server name once the app's ClientHello names it, or
/// under its address when no name comes within [`SNI_WAIT`].
async fn handle_tcp(flow: IpStackTcpStream, router: Arc<Router>, dns: Option<DnsService>) {
    let Some(dst) = ipv4_dst(flow.peer_addr()) else {
        return;
    };
    if let Some(handler) = DnsService::handler_for(dns, dst) {
        return dns::serve_tcp_flow(flow, handler).await;
    }

    let address = dst.ip().to_string();
    let request = OutboundRequest {
        host: OutboundHost::Resolved {
            domain: address.clone(),
            ip: Some(*dst.ip()),
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
    let Some(dst) = ipv4_dst(flow.peer_addr()) else {
        return;
    };
    if let Some(handler) = DnsService::handler_for(dns, dst) {
        return dns::serve_udp_flow(flow, handler).await;
    }
    if let Err(e) = relay_udp(flow, dst).await {
        tracing::debug!("UDP flow to {dst} ended: {e}");
    }
}

/// Relays one UDP flow through its own direct socket until ipstack closes it as idle.
async fn relay_udp(mut flow: IpStackUdpStream, dst: SocketAddrV4) -> std::io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
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
        let virtual_server = SocketAddrV4::new(DNS_ADDR, 53);
        let public = SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
        let other_port = SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 5353);

        assert!(DnsIntercept::VirtualServer.matches(virtual_server));
        assert!(!DnsIntercept::VirtualServer.matches(public));
        assert!(DnsIntercept::AnyServer.matches(virtual_server));
        assert!(DnsIntercept::AnyServer.matches(public));
        assert!(!DnsIntercept::AnyServer.matches(other_port));
        assert!(!DnsIntercept::VirtualServer.matches(SocketAddrV4::new(DNS_ADDR, 80)));
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
                assert_eq!(ip, Some(Ipv4Addr::LOCALHOST));
                assert_eq!(outbound, "direct");
            }
            OutboundEvent::Error { error, .. } => panic!("connecting failed: {error}"),
        }
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
