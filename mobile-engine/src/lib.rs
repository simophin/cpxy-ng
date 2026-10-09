//! The packet engine of the mobile VPN app: reads IP packets from the platform's TUN device and
//! does what `cpxy-router` does on OpenWrt (see `docs/mobile-vpn-plan.md`):
//!
//! - DNS sent to [`DNS_ADDR`] is answered with `dns_split`.
//! - TCP to private and CN addresses goes direct, everything else through the cpxy server. When
//!   the server is unreachable the connection fails; it is never sent direct instead.
//! - UDP 443 (QUIC) and TCP 853 (DNS over TLS) are refused; other UDP is relayed direct.
//! - IPv6 is refused.
//!
//! The engine's own sockets must bypass the tunnel; the platform arranges that.

mod config;
mod dns;
mod ffi;
mod filter;
#[cfg(target_os = "linux")]
pub mod linux;
mod routing;
mod tun;

pub use client::stats_server::OutboundEvent;
pub use config::{Config, DEFAULT_MTU, DNS_ADDR, TUN_ADDR, TUN_PREFIX_LEN};
pub use ffi::{ConnectionEvent, Engine, EngineError, EngineListener, start_engine};

uniffi::setup_scaffolding!();

use client::dns_split::server::DnsSplitHandler;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use ipstack::{
    IpStack, IpStackConfig, IpStackStream, IpStackTcpStream, IpStackUdpStream, TcpConfig,
};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::fd::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;
use tokio::sync::{broadcast, oneshot};
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
/// How long stopping waits for the flows to end, and then for the runtime to shut down.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

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
    let settings = Config::from_json(config_json)?.settings()?;
    start_inner(
        tun,
        settings.server,
        settings.mtu,
        listener,
        Some((&settings.dns_upstream, &settings.dns_alternative)),
    )
}

/// Starts the shared packet engine for a router. DNS stays with dnsmasq/dns_split;
/// no synthetic DNS address is intercepted and no SOCKS listener is needed.
pub fn start_router(
    tun: OwnedFd,
    server: client::protocol_config::Config,
    mtu: u16,
    listener: Arc<dyn EventListener>,
) -> anyhow::Result<EngineHandle> {
    start_inner(tun, server, mtu, listener, None)
}

fn start_inner(
    tun: OwnedFd,
    server: client::protocol_config::Config,
    mtu: u16,
    listener: Arc<dyn EventListener>,
    dns_specs: Option<(
        &[client::dns_split::spec::ServerSpec],
        &[client::dns_split::spec::ServerSpec],
    )>,
) -> anyhow::Result<EngineHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("cpxy-engine")
        .enable_all()
        .build()?;
    let dns = dns_specs
        .map(|(upstream, alternative)| {
            runtime
                .block_on(dns::handler(upstream, alternative))
                .map(Arc::new)
        })
        .transpose()?;
    let traffic = Arc::new(Traffic::default());
    let (device_closed_tx, device_closed) = oneshot::channel();
    let device = {
        let _guard = runtime.enter();
        TunDevice::new(tun, mtu, traffic.clone(), device_closed_tx)?
    };

    let (events_tx, events_rx) = broadcast::channel(256);
    let outbound = Arc::new(routing::outbound(server, events_tx));
    let (shutdown, shutdown_rx) = oneshot::channel();
    runtime.spawn(forward_events(events_rx, listener));
    let stopped = runtime.spawn(run(device, device_closed, mtu, outbound, dns, shutdown_rx));
    tracing::info!("Engine started");

    Ok(EngineHandle {
        running: Mutex::new(Some(Running {
            runtime,
            shutdown,
            stopped,
        })),
        traffic,
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

async fn run<O: Outbound + Send + Sync + 'static>(
    device: TunDevice,
    device_closed: oneshot::Receiver<()>,
    mtu: u16,
    outbound: Arc<O>,
    dns: Option<Arc<DnsSplitHandler>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut tcp_config = TcpConfig::default();
    tcp_config.timeout = TCP_IDLE_TIMEOUT;
    let mut config = IpStackConfig::default();
    config
        .mtu_unchecked(mtu)
        .udp_timeout(UDP_IDLE_TIMEOUT)
        .with_tcp_config(tcp_config);
    let mut stack = IpStack::new(config, device);
    let mut flows = JoinSet::new();

    loop {
        let stream = tokio::select! {
            stream = stack.accept() => stream,
            // Reap the flows that ended.
            Some(_) = flows.join_next() => continue,
            _ = &mut shutdown => break,
        };
        match stream {
            Ok(IpStackStream::Tcp(tcp)) => {
                flows.spawn(handle_tcp(tcp, outbound.clone(), dns.clone()));
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
    flows.shutdown().await;
    let _ = device_closed.await;
}

fn ipv4_dst(addr: SocketAddr) -> Option<SocketAddrV4> {
    match addr {
        SocketAddr::V4(addr) => Some(addr),
        // The filter refuses IPv6 before it reaches the stack.
        SocketAddr::V6(_) => None,
    }
}

async fn handle_tcp<O: Outbound>(
    mut flow: IpStackTcpStream,
    outbound: Arc<O>,
    dns: Option<Arc<DnsSplitHandler>>,
) {
    let Some(dst) = ipv4_dst(flow.peer_addr()) else {
        return;
    };
    if is_dns(dst)
        && let Some(dns) = dns
    {
        return dns::serve_tcp_flow(flow, dns).await;
    }

    let request = OutboundRequest {
        host: OutboundHost::Resolved {
            domain: dst.ip().to_string(),
            ip: Some(*dst.ip()),
        },
        port: dst.port(),
        tls: false,
        initial_plaintext: Vec::new(),
    };
    // On failure the flow is dropped, which resets the app's connection.
    let Ok(mut upstream) = outbound.send(request).await else {
        return;
    };
    if let Err(e) = tokio::io::copy_bidirectional(&mut flow, &mut upstream).await {
        tracing::debug!("TCP flow to {dst} ended: {e}");
    }
}

async fn handle_udp(flow: IpStackUdpStream, dns: Option<Arc<DnsSplitHandler>>) {
    let Some(dst) = ipv4_dst(flow.peer_addr()) else {
        return;
    };
    if is_dns(dst)
        && let Some(dns) = dns
    {
        return dns::serve_udp_flow(flow, dns).await;
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

fn is_dns(dst: SocketAddrV4) -> bool {
    *dst.ip() == DNS_ADDR && dst.port() == DNS_PORT
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
        assert!(!is_open(raw), "the TUN descriptor is still open");
    }
}
