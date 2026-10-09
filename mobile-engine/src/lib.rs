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
mod filter;
mod routing;
mod tun;

pub use client::stats_server::OutboundEvent;
pub use config::{Config, DEFAULT_MTU, DNS_ADDR, TUN_ADDR, TUN_PREFIX_LEN};

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

/// Receives what the engine reports while it runs. Called from engine threads.
pub trait EventListener: Send + Sync {
    /// A TCP flow was connected direct or through the proxy, or failed to connect.
    fn on_outbound_event(&self, event: OutboundEvent);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrafficStats {
    /// IP bytes from the device into the tunnel.
    pub sent: u64,
    /// IP bytes from the tunnel to the device.
    pub received: u64,
}

pub struct EngineHandle {
    runtime: Mutex<Option<Runtime>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    traffic: Arc<Traffic>,
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("cpxy-engine")
        .enable_all()
        .build()?;

    let dns = Arc::new(runtime.block_on(dns::handler(
        &settings.dns_upstream,
        &settings.dns_alternative,
    ))?);
    let traffic = Arc::new(Traffic::default());
    let device = {
        let _guard = runtime.enter();
        TunDevice::new(tun, settings.mtu, traffic.clone())?
    };

    let (events_tx, events_rx) = broadcast::channel(256);
    let outbound = Arc::new(routing::outbound(settings.server, events_tx));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    runtime.spawn(forward_events(events_rx, listener));
    runtime.spawn(run(device, settings.mtu, outbound, dns, shutdown_rx));
    tracing::info!("Engine started");

    Ok(EngineHandle {
        runtime: Mutex::new(Some(runtime)),
        shutdown: Mutex::new(Some(shutdown_tx)),
        traffic,
    })
}

impl EngineHandle {
    /// Stops the engine and closes the TUN descriptor. Must not be called from an engine thread,
    /// such as an [`EventListener`] callback.
    pub fn stop(&self) {
        if let Some(shutdown) = self.shutdown.lock().unwrap().take() {
            let _ = shutdown.send(());
        }
        if let Some(runtime) = self.runtime.lock().unwrap().take() {
            runtime.shutdown_timeout(Duration::from_secs(2));
            tracing::info!("Engine stopped");
        }
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
    mtu: u16,
    outbound: Arc<O>,
    dns: Arc<DnsSplitHandler>,
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

    loop {
        let stream = tokio::select! {
            stream = stack.accept() => stream,
            _ = &mut shutdown => return,
        };
        match stream {
            Ok(IpStackStream::Tcp(tcp)) => {
                tokio::spawn(handle_tcp(tcp, outbound.clone(), dns.clone()));
            }
            Ok(IpStackStream::Udp(udp)) => {
                tokio::spawn(handle_udp(udp, dns.clone()));
            }
            // ICMP and anything else the filter let through: not relayed.
            Ok(IpStackStream::UnknownTransport(_) | IpStackStream::UnknownNetwork(_)) => {}
            Err(e) => {
                tracing::error!("IP stack stopped: {e}");
                return;
            }
        }
    }
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
    dns: Arc<DnsSplitHandler>,
) {
    let Some(dst) = ipv4_dst(flow.peer_addr()) else {
        return;
    };
    if is_dns(dst) {
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

async fn handle_udp(flow: IpStackUdpStream, dns: Arc<DnsSplitHandler>) {
    let Some(dst) = ipv4_dst(flow.peer_addr()) else {
        return;
    };
    if is_dns(dst) {
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
