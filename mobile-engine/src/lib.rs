//! The packet engine of the mobile VPN app: reads IP packets from the platform's TUN device and
//! does what the `cpxy` package does on OpenWrt (see `docs/mobile-vpn-plan.md`):
//!
//! - DNS sent to [`DNS_ADDR`] is answered with `dns_split`. A router has no TUN: it answers its
//!   networks' DNS on its own sockets with `cpxy dns --resolver-routing cn_split`.
//! - TCP to private and CN addresses goes direct, everything else through the cpxy server. When
//!   the server is unreachable the connection fails; it is never sent direct instead.
//! - Captured UDP 443 (QUIC) is refused; other UDP is relayed direct. Legacy mode also
//!   refuses TCP 853 (DNS over TLS); Android SOCKS mode follows normal TCP routing.
//! - IPv6 is handled like IPv4, or refused, according to the config's [`Ipv6Mode`]. Android SOCKS
//!   mode always refuses it, so apps use IPv4; the proxy transport may still use IPv6.
//!
//! The engine's own sockets must bypass the tunnel; the platform arranges that.
//!
//! TCP is terminated by a userspace TCP stack (smoltcp, through `netstack-smoltcp`). UDP is
//! relayed datagram by datagram (see `udp.rs`).

mod android_routes;
mod config;
mod dns;
mod ffi;
mod filter;
#[cfg(target_os = "linux")]
pub mod linux;
mod proxy;
mod routing;
mod tun;
mod udp;

pub use config::{
    Config, DEFAULT_MTU, DNS_ADDR, TUN_ADDR, TUN_ADDR_V6, TUN_PREFIX_LEN, TUN_PREFIX_LEN_V6,
};
pub use ffi::{Engine, EngineError, start_engine};

uniffi::setup_scaffolding!();

use client::dns_split::server::DnsSplitHandler;
pub use client::ipv6::Ipv6Mode;
use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
use routing::Router;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use netstack_smoltcp::{Stack, StackBuilder, TcpStream};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tun::{Traffic, Tun};

/// Small, to stay within the iOS packet tunnel extension's memory limit.
const WORKER_THREADS: usize = 2;
/// Idle UDP flows are closed, and their direct sockets released, after this long.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const DNS_PORT: u16 = 53;
/// What a TCP flow buffers each way, which is also the window it advertises: a flow moves at
/// most this much per round trip between the device's apps and the engine. The apps are a
/// millisecond away, and memory is tight.
const TCP_WINDOW: u32 = 64 << 10;
/// How long stopping waits for the flows to end, and then for the runtime to shut down.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrafficStats {
    /// IP bytes from the device into the tunnel.
    pub sent: u64,
    /// IP bytes from the tunnel to the device.
    pub received: u64,
}

pub struct EngineHandle {
    running: Mutex<Option<Running>>,
    traffic: Arc<Traffic>,
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
pub fn start(tun: OwnedFd, config_json: &str) -> anyhow::Result<EngineHandle> {
    let settings = Config::from_json(config_json)?.settings()?;
    start_inner(
        tun,
        if settings.android_socks {
            match settings.socks5 {
                Some(address) => Backend::AndroidExternal(address),
                None => Backend::AndroidCpxy(settings.server.expect("validated cpxy server")),
            }
        } else {
            Backend::Cpxy(settings.server.expect("validated cpxy server"))
        },
        settings.proxy_routes,
        settings.ipv6,
        settings.mtu,
        proxy::Ports {
            http: settings.http_proxy_port,
            socks: settings.socks_proxy_port,
            local_http: true,
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
            intercept: true,
        }),
    )
}

struct DnsSpecs<'a> {
    split: dns::Specs<'a>,
    /// Whether DNS from the TUN to [`DNS_ADDR`] is answered; otherwise it is all relayed.
    intercept: bool,
}

/// The DNS the engine answers, and how.
#[derive(Clone)]
struct DnsService {
    handler: Arc<DnsSplitHandler>,
    intercept: bool,
}

impl DnsService {
    /// The handler to answer a flow to `dst` with, if the engine answers it.
    fn handler_for(dns: Option<Self>, dst: SocketAddr) -> Option<Arc<DnsSplitHandler>> {
        dns.filter(|dns| dns.intercept && dst == SocketAddr::from((DNS_ADDR, DNS_PORT)))
            .map(|dns| dns.handler)
    }
}

enum Backend {
    Cpxy(client::protocol_config::Config),
    AndroidCpxy(client::protocol_config::Config),
    AndroidExternal(SocketAddr),
}

#[allow(clippy::too_many_arguments)]
fn start_inner(
    tun: OwnedFd,
    server: Backend,
    proxy_routes: client::outbound::ProxyRoutes,
    ipv6: Ipv6Mode,
    mtu: u16,
    proxy_ports: proxy::Ports,
    dns_specs: Option<DnsSpecs>,
) -> anyhow::Result<EngineHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("cpxy-engine")
        .enable_all()
        .build()?;
    // Bind and spawn on a live multi-thread runtime before resolver initialization can dial it.
    // The task and all accepted connections are owned by this runtime, including error paths.
    let android = matches!(
        &server,
        Backend::AndroidCpxy(_) | Backend::AndroidExternal(_)
    );
    let mut embedded_task = None;
    let server = match server {
        Backend::AndroidCpxy(config) => {
            let (address, task) = runtime.block_on(proxy::embedded(config))?;
            embedded_task = Some(task);
            Backend::AndroidExternal(address)
        }
        server => server,
    };
    let dns = dns_specs
        .as_ref()
        .map(|specs| {
            runtime
                .block_on(async {
                    match &server {
                        Backend::Cpxy(server) => dns::handler(&specs.split, server).await,
                        Backend::AndroidExternal(address) => {
                            dns::router_handler(&specs.split, *address, &proxy_routes).await
                        }
                        Backend::AndroidCpxy(_) => unreachable!(),
                    }
                })
                .map(|handler| DnsService {
                    handler: Arc::new(handler),
                    intercept: specs.intercept,
                })
        })
        .transpose()?;
    // Bind before reporting success, so an occupied port fails the connection visibly.
    let proxies = runtime.block_on(proxy::Listeners::bind(proxy_ports))?;
    let local_proxy_port = proxies.local_http_port();
    let traffic = Arc::new(Traffic::default());
    let (device_closed_tx, device_closed) = oneshot::channel();
    let filter = filter::Filter::new(ipv6, proxy_routes.clone()).with_tcp_dot_allowed(android);
    let device = {
        let _guard = runtime.enter();
        Tun::new(tun, device_closed_tx)?
    };

    let router = Arc::new(match server {
        Backend::Cpxy(server) => Router::new(server, proxy_routes),
        Backend::AndroidExternal(address) => Router::split_socks5(address, proxy_routes),
        Backend::AndroidCpxy(_) => unreachable!(),
    });
    let (shutdown, shutdown_rx) = oneshot::channel();
    let stopped = runtime.spawn(run(
        device,
        device_closed,
        filter,
        traffic.clone(),
        mtu,
        router,
        dns,
        proxies,
        embedded_task,
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
        local_proxy_port,
    })
}

impl EngineHandle {
    /// Stops the engine and closes the TUN descriptor. Must not be called from an engine thread,
    /// including callbacks on its runtime.
    pub fn stop(&self) {
        let Some(running) = self.running.lock().unwrap().take() else {
            return;
        };
        let _ = running.shutdown.send(());
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
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    device: Tun,
    device_closed: oneshot::Receiver<()>,
    filter: filter::Filter,
    traffic: Arc<Traffic>,
    mtu: u16,
    router: Arc<Router>,
    dns: Option<DnsService>,
    proxies: proxy::Listeners,
    embedded_task: Option<JoinHandle<anyhow::Result<()>>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let built = StackBuilder::default()
        .enable_tcp(true)
        .mtu(mtu.into())
        .tcp_recv_buffer_size(TCP_WINDOW)
        .tcp_send_buffer_size(TCP_WINDOW)
        // The apps are on the same device: there is no path to keep alive.
        .tcp_keep_alive(None)
        .build();
    let (stack, Some(runner), _, Some(mut tcp)) = (match built {
        Ok(built) => built,
        Err(error) => {
            tracing::error!(%error, "Setting up the TCP stack failed");
            return;
        }
    }) else {
        unreachable!("TCP is enabled");
    };
    let (to_stack, from_stack) = stack.split();

    let mut listener_tasks =
        proxies.spawn(router.clone(), dns.as_ref().map(|dns| dns.handler.clone()));
    if let Some(task) = embedded_task {
        listener_tasks.spawn(async move { task.await? });
    }
    let mut stack_tasks = JoinSet::new();
    stack_tasks.spawn(runner);
    stack_tasks.spawn(to_device(from_stack, device.clone(), traffic.clone()));
    let mut packets = tokio::spawn(pump(device, filter, to_stack, traffic, mtu, dns.clone()));
    let mut flows = JoinSet::new();

    loop {
        tokio::select! {
            accepted = tcp.next() => {
                let Some((stream, _, dst)) = accepted else {
                    tracing::error!("The TCP stack stopped");
                    break;
                };
                flows.spawn(handle_tcp(stream, dst, router.clone(), dns.clone()));
            }
            // Reap the flows that ended.
            Some(_) = flows.join_next() => {}
            Some(result) = listener_tasks.join_next() => {
                tracing::error!(?result, "Proxy or DNS listener stopped");
                break;
            }
            Some(result) = stack_tasks.join_next() => {
                tracing::error!(?result, "The TCP stack stopped");
                break;
            }
            _ = &mut shutdown => break,
            result = &mut packets => {
                match result {
                    Ok(Ok(())) => tracing::warn!("The device closed"),
                    result => tracing::error!(?result, "Reading the device failed"),
                }
                break;
            }
        }
    }

    // The packet pump owns the UDP flows: aborting it ends them.
    packets.abort();
    let _ = packets.await;
    stack_tasks.shutdown().await;
    listener_tasks.shutdown().await;
    flows.shutdown().await;
    let _ = device_closed.await;
}

/// Moves the packets the device sends into the tunnel: TCP goes to the TCP stack, UDP to its
/// flow. Returns when the device closes.
async fn pump(
    device: Tun,
    filter: filter::Filter,
    mut to_stack: SplitSink<Stack, Vec<u8>>,
    traffic: Arc<Traffic>,
    mtu: u16,
    dns: Option<DnsService>,
) -> std::io::Result<()> {
    const PROTO_TCP: u8 = 6;
    const PROTO_UDP: u8 = 17;
    let mut udp = udp::Flows::new(device.clone(), mtu, traffic.clone());
    let mut udp_flows = JoinSet::new();
    let mut buf = vec![0u8; usize::from(mtu)];
    loop {
        let n = tokio::select! {
            n = device.recv(&mut buf) => n?,
            Some(_) = udp_flows.join_next() => continue,
        };
        let Some(n) = n else { return Ok(()) };
        let packet = &buf[..n];
        traffic.sent.fetch_add(n as u64, Ordering::Relaxed);
        match filter.classify(packet) {
            filter::Verdict::Pass => {}
            filter::Verdict::Drop => continue,
            filter::Verdict::Reply(reply) => {
                traffic
                    .received
                    .fetch_add(reply.len() as u64, Ordering::Relaxed);
                if let Err(error) = device.send(&reply).await {
                    tracing::debug!(%error, "Error writing ICMP reply");
                }
                continue;
            }
        }
        match ip_protocol(packet) {
            Some(PROTO_TCP) => to_stack.send(packet.to_vec()).await?,
            Some(PROTO_UDP) => {
                if let Some(flow) = udp.dispatch(packet) {
                    udp_flows.spawn(handle_udp(flow, dns.clone()));
                }
            }
            // ICMP and anything else the filter let through: not relayed.
            _ => {}
        }
    }
}

/// Writes the TCP stack's packets into the device.
async fn to_device(
    mut from_stack: SplitStream<Stack>,
    device: Tun,
    traffic: Arc<Traffic>,
) -> std::io::Result<()> {
    while let Some(packet) = from_stack.next().await {
        let packet = packet?;
        traffic
            .received
            .fetch_add(packet.len() as u64, Ordering::Relaxed);
        // A packet that cannot be written is lost, as on any link.
        if let Err(error) = device.send(&packet).await {
            tracing::debug!(%error, "Error writing a packet to the device");
        }
    }
    Ok(())
}

/// The transport protocol of an IP packet; IPv6 extension headers are not followed.
fn ip_protocol(packet: &[u8]) -> Option<u8> {
    match packet.first()? >> 4 {
        4 => packet.get(9).copied(),
        6 => packet.get(6).copied(),
        _ => None,
    }
}

async fn handle_tcp(
    mut flow: TcpStream,
    dst: SocketAddr,
    router: Arc<Router>,
    dns: Option<DnsService>,
) {
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
    let mut upstream = match cpxy_ng::outbound::Outbound::send(router.as_ref(), request).await {
        Ok(upstream) => upstream,
        Err(error) => {
            tracing::warn!(%dst, %error, "TCP connection failed");
            return;
        }
    };
    if let Err(error) =
        cpxy_ng::relay::relay(&mut flow, &mut upstream, cpxy_ng::relay::DEFAULT_LINGER).await
    {
        tracing::debug!(%dst, %error, "TCP flow ended");
    }
}

async fn handle_udp(flow: udp::Flow, dns: Option<DnsService>) {
    let dst = flow.peer_addr();
    if let Some(handler) = DnsService::handler_for(dns, dst) {
        return dns::serve_udp_flow(flow, handler).await;
    }
    if let Err(e) = relay_udp(flow, dst).await {
        tracing::debug!("UDP flow to {dst} ended: {e}");
    }
}

/// Relays one UDP flow through its own direct socket until it has been idle for
/// [`UDP_IDLE_TIMEOUT`].
async fn relay_udp(mut flow: udp::Flow, dst: SocketAddr) -> std::io::Result<()> {
    let unspecified: IpAddr = match dst {
        SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let socket = UdpSocket::bind((unspecified, 0)).await?;
    socket.connect(dst).await?;

    let mut from_remote = vec![0u8; 65535];
    loop {
        tokio::select! {
            packet = flow.recv() => {
                let Some(packet) = packet else { return Ok(()) };
                socket.send(&packet).await?;
            }
            n = socket.recv(&mut from_remote) => {
                if let Err(error) = flow.send(&from_remote[..n?]).await {
                    if error.kind() != std::io::ErrorKind::InvalidInput { return Err(error); }
                    tracing::debug!(%dst, "Dropped UDP reply exceeding TUN MTU");
                }
            }
            _ = tokio::time::sleep(UDP_IDLE_TIMEOUT) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};

    #[test]
    fn android_udp_preserves_empty_and_large_datagrams_with_dead_socks() {
        use std::os::unix::net::UnixDatagram;
        // IPv4 only: Android refuses IPv6 from apps (see `relays_ipv6_unless_it_is_off`).
        let endpoint = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        endpoint
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let destination = endpoint.local_addr().unwrap();
        let (tun, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let config = r#"{"android_socks":true,"socks5":"127.0.0.1:9","dns_upstream":["127.0.0.1"],"dns_alternative":["127.0.0.1"]}"#;
        let engine = start(tun.into(), config).unwrap();
        for (index, size) in [0, 1400].into_iter().enumerate() {
            let builder = etherparse::PacketBuilder::ipv4(
                TUN_ADDR.octets(),
                Ipv4Addr::LOCALHOST.octets(),
                64,
            )
            .udp(40000 + index as u16, destination.port());
            let payload = vec![7; size];
            let mut packet = Vec::new();
            builder.write(&mut packet, &payload).unwrap();
            peer.send(&packet).unwrap();
            let mut buffer = [0; 2048];
            let (n, sender) = endpoint.recv_from(&mut buffer).unwrap();
            assert_eq!(n, size);
            endpoint.send_to(&buffer[..n], sender).unwrap();
            let n = peer.recv(&mut buffer).unwrap();
            let packet = etherparse::SlicedPacket::from_ip(&buffer[..n]).unwrap();
            let Some(etherparse::TransportSlice::Udp(udp)) = packet.transport else {
                panic!("not UDP")
            };
            assert_eq!(udp.payload(), payload);
        }
        engine.stop();
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
        let engine = start(tun, config).unwrap();
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
    fn stopping_engine_closes_proxy_listeners_and_pending_handshakes() {
        check_proxy_shutdown(false);
        check_proxy_shutdown(true);
    }

    fn check_proxy_shutdown(android: bool) {
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
            r#"{{"server":"http://:k@127.0.0.1:1", "dns_upstream":["127.0.0.1"], "dns_alternative":["127.0.0.1"], "socks_proxy_port":{port}, "android_socks":{android}}}"#
        );
        let engine = start(tun, &config).unwrap();
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
        let engine = start(tun.into(), &config("direct")).unwrap();
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
        let engine = start(tun.into(), &config("off")).unwrap();
        peer.send(&packet_to(tcp_port, Some((1, None)), &[]))
            .unwrap();
        let n = peer.recv(&mut buf).unwrap();
        assert!(matches!(
            etherparse::SlicedPacket::from_ip(&buf[..n])
                .unwrap()
                .transport,
            Some(etherparse::TransportSlice::Icmpv6(_))
        ));
        engine.stop();

        // Android never relays IPv6 from apps, whatever the config asks for.
        let (tun, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let android = r#"{"android_socks": true, "socks5": "127.0.0.1:1", "dns_upstream": ["127.0.0.1"], "dns_alternative": ["127.0.0.1"], "ipv6": "full"}"#;
        let engine = start(tun.into(), android).unwrap();
        for packet in [
            packet_to(tcp_port, Some((1, None)), &[]),
            packet_to(udp_port, None, b"ping"),
        ] {
            peer.send(&packet).unwrap();
            let n = peer.recv(&mut buf).unwrap();
            assert!(matches!(
                etherparse::SlicedPacket::from_ip(&buf[..n])
                    .unwrap()
                    .transport,
                Some(etherparse::TransportSlice::Icmpv6(_))
            ));
        }
        echo.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        assert!(echo.recv_from(&mut buf).is_err(), "IPv6 UDP was relayed");
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
        let engine = start(tun, config).unwrap();
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
