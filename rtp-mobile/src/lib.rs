//! The RTP VPN app's engine: carries the device's IPv4 traffic through an `rtp-l3` tunnel to a
//! `cpxy-rtp endpoint --internet-exit`, answers its DNS with the DNS split and refuses what the
//! tunnel cannot carry. The platform routes only the non-direct addresses
//! ([`tunnel_routes`]) into the TUN, so direct traffic never reaches the engine.
//!
//! ```text
//! TUN ⇄ shim ⇄ packet channels ⇄ rtp-l3 client worker ⇄ UDP to the endpoint
//! ```

uniffi::setup_scaffolding!();

mod connect;
mod dns;
mod ffi;
pub mod packet;
pub mod profile;
mod shim;

use ipnet::Ipv4Net;
use profile::Profile;
use rtp_l3::{
    v2::{
        client_runtime::{self, ClientConfig, PathCounters},
        http_client::Client,
    },
    worker::SocketProvider,
};
use shim::{Counters, Datagrams, IoCounts, Shim};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The virtual DNS server the TUN advertises. In the benchmarking range, which is not direct, so
/// it is inside [`tunnel_routes`]; the engine answers it and never sends it to the endpoint.
pub const DNS_ADDRESS: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 53);
/// The tunnel's inner packet size.
pub const MTU: u16 = rtp_l3::MTU as u16;
/// Direct prefixes longer than this go through the tunnel, keeping the route list at about 2,100
/// routes instead of 18,000. Such traffic still works; it is just not direct.
const LONGEST_DIRECT_PREFIX: u8 = 18;
/// The protocol default: a PING when idle this long, with jitter.
const KEEPALIVE_SECONDS: u64 = 15;
/// The utun packet-information header is only used on Apple platforms.
const FAMILY_HEADER: bool = cfg!(any(target_os = "ios", target_os = "macos"));

/// The routes to install into the TUN: every IPv4 address outside the direct set.
pub fn tunnel_routes() -> Vec<Ipv4Net> {
    geoip_data::routes::tunnel_ipv4(&[], LONGEST_DIRECT_PREFIX)
        .into_iter()
        .map(|(ip, prefix)| Ipv4Net::new(ip.into(), prefix).unwrap())
        .collect()
}

pub trait Platform: Send + Sync + 'static {
    /// Makes `socket` bypass the VPN before anything is sent on it.
    fn protect_socket(&self, fd: RawFd) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Connecting,
    Connected,
    Repairing,
    Degraded,
    /// The worker ended with an error; the platform should start again.
    Failed,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub state: State,
    /// An `io::ErrorKind` when the worker failed; OS messages are not kept.
    pub error: Option<String>,
    pub sent_packets: u64,
    pub sent_bytes: u64,
    pub received_packets: u64,
    pub received_bytes: u64,
    pub dns_queries: u64,
    pub refused: u64,
    pub repairs: u64,
    pub fallbacks: u64,
    /// The worker's packet-path counts as of its latest report, about every 10 seconds.
    pub worker: Option<PathCounters>,
    /// Shim calls on the TUN.
    pub shim_tun: IoCounts,
}

#[derive(Default)]
struct WorkerStatus {
    state: Option<State>,
    error: Option<String>,
    repairs: u64,
    fallbacks: u64,
    path: Option<PathCounters>,
}

impl WorkerStatus {
    fn observe(&mut self, event: &serde_json::Value) {
        let state = match event["event"].as_str() {
            Some("connected" | "repaired" | "path_alive") => State::Connected,
            Some("repairing") => State::Repairing,
            Some("degraded") => State::Degraded,
            Some("stats" | "final") => {
                let count = |name: &str| event[name].as_u64().unwrap_or(0);
                self.repairs = count("active_repairs");
                self.fallbacks = count("fallbacks");
                if let Ok(path) = serde_json::from_value(event["path"].clone()) {
                    self.path = Some(path);
                }
                return;
            }
            _ => return,
        };
        self.state = Some(state);
    }
}

pub struct Engine {
    stop: CancellationToken,
    repair: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    status: Arc<Mutex<WorkerStatus>>,
    counters: Arc<Counters>,
    provider: Arc<Provider>,
    dns: Arc<client::dns_split::server::DnsSplitHandler>,
}

/// A prepared control session. Address allocation and protected HTTP happen
/// before the platform establishes TUN; no transport keys/counters are reused.
pub struct Prepared {
    settings: profile::Settings,
    endpoint: SocketAddr,
    inner: Ipv4Addr,
    path: client_runtime::PreparedPath,
    provider: Arc<Provider>,
}
pub fn prepare(
    profile: &Profile,
    key: [u8; 32],
    client_id: uuid::Uuid,
    platform: Arc<dyn Platform>,
) -> anyhow::Result<Prepared> {
    anyhow::ensure!(!client_id.is_nil(), "Invalid persisted client UUID");
    let settings = profile.settings()?;
    let endpoints = settings.resolve_endpoints()?;
    let alternative = settings.dns_alternative.clone();
    let (path, provider) = connect::race(endpoints, move |endpoint, cancelled| {
        let provider = Arc::new(Provider {
            platform: platform.clone(),
            repair: Arc::new(AtomicBool::new(false)),
            stopping: AtomicBool::new(false),
            control: Mutex::new(None),
        });
        let control = Client::new(&key, "cpxy-rtp", endpoint, provider.clone(), client_id)?;
        let path = client_runtime::prepare_path(
            control,
            provider.as_ref(),
            &cancelled,
            KEEPALIVE_SECONDS,
        )?;
        anyhow::ensure!(
            !alternative.contains(&path.inner()) && path.inner() != DNS_ADDRESS,
            "Allocated address conflicts with configured DNS"
        );
        Ok((path, provider))
    })?;
    let endpoint = path.entry();
    let inner = path.inner();
    tracing::info!(%endpoint, "Selected authenticated RTP UDP path");
    Ok(Prepared {
        settings,
        endpoint,
        inner,
        path,
        provider,
    })
}
impl Prepared {
    pub fn address(&self) -> Ipv4Addr {
        self.inner
    }
    pub fn start(self, tun: OwnedFd) -> anyhow::Result<Engine> {
        self.start_inner(tun, false)
    }
    /// OpenWrt forwards regional IPv6 in the kernel; the RTP TUN stays IPv4-only.
    pub fn start_direct_ipv6(self, tun: OwnedFd) -> anyhow::Result<Engine> {
        self.start_inner(tun, true)
    }
    fn start_inner(self, tun: OwnedFd, direct_ipv6: bool) -> anyhow::Result<Engine> {
        let Self {
            settings,
            endpoint,
            inner,
            path,
            provider,
        } = self;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rtp-engine")
            .enable_all()
            .build()?;
        let (device, port) = rtp_l3::device::channel();
        let counters = Arc::new(Counters::default());
        let shim = runtime.block_on(async {
            let tun = Datagrams::new(tun, FAMILY_HEADER, counters.tun.clone())?;
            let tunnel = port.reads;
            let queries = Arc::new(dns::TunnelQueries::new(inner, tunnel.clone()));
            let handler = dns::handler(&settings, queries.clone(), direct_ipv6).await?;
            anyhow::Ok(Arc::new(Shim {
                tun,
                tunnel,
                own: inner,
                dns: DNS_ADDRESS,
                resolvers: settings.dns_alternative.clone(),
                handler: Arc::new(handler),
                queries,
                counters: counters.clone(),
            }))
        })?;

        let repair = provider.repair.clone();
        let status = Arc::new(Mutex::new(WorkerStatus::default()));
        let stop = CancellationToken::new();
        let config = ClientConfig {
            entry: endpoint,
            inner,
            keepalive_seconds: KEEPALIVE_SECONDS,
        };
        let events_status = status.clone();
        let events: rtp_l3::worker::Events = Arc::new(move |event: serde_json::Value| {
            tracing::debug!("{event}");
            events_status.lock().unwrap().observe(&event);
        });
        let worker_provider = provider.clone();
        let worker_status = status.clone();
        let worker = runtime.spawn(client_runtime::run_prepared(
            config,
            path,
            device,
            worker_provider,
            events,
            stop.clone(),
        ));
        let worker = runtime.spawn(async move {
            let result = worker
                .await
                .unwrap_or_else(|_| Err(io::Error::other("tunnel worker panicked")));
            let mut status = worker_status.lock().unwrap();
            match result {
                Ok(()) => status.state = Some(State::Stopped),
                Err(e) => {
                    tracing::warn!("The tunnel worker failed: {e}");
                    status.error = Some(format!("{:?}", e.kind()));
                    status.state = Some(State::Failed);
                }
            }
        });

        let dns = shim.handler.clone();
        runtime.spawn(shim.clone().outbound());
        runtime.spawn(shim.inbound(port.writes));
        Ok(Engine {
            stop,
            repair,
            worker: Mutex::new(Some(worker)),
            runtime: Mutex::new(Some(runtime)),
            status,
            counters,
            provider,
            dns,
        })
    }
}

impl Engine {
    /// Open scoped router DNS listeners on the same split policy as the packet shim.
    /// Alternative DNS continues through injected tunnel queries, including TCP clients.
    pub fn serve_dns(&self, address: SocketAddr) -> anyhow::Result<()> {
        let runtime = self.runtime.lock().unwrap();
        let runtime = runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Engine stopped"))?;
        let (udp, tcp) = runtime.block_on(async {
            let udp = tokio::net::UdpSocket::bind(address).await?;
            let tcp = tokio::net::TcpListener::bind(address).await?;
            anyhow::Ok((udp, tcp))
        })?;
        runtime.spawn(client::dns_split::server::serve_udp(udp, self.dns.clone()));
        runtime.spawn(client::dns_split::server::serve_tcp(tcp, self.dns.clone()));
        Ok(())
    }
    pub fn status(&self) -> Status {
        let worker = self.status.lock().unwrap();
        let count = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
        Status {
            state: worker.state.unwrap_or(State::Connecting),
            error: worker.error.clone(),
            sent_packets: count(&self.counters.sent_packets),
            sent_bytes: count(&self.counters.sent_bytes),
            received_packets: count(&self.counters.received_packets),
            received_bytes: count(&self.counters.received_bytes),
            dns_queries: count(&self.counters.dns_queries),
            refused: count(&self.counters.refused),
            repairs: worker.repairs,
            fallbacks: worker.fallbacks,
            worker: worker.path,
            shim_tun: self.counters.tun.counts(),
        }
    }

    /// The device's network changed: replace the tunnel's path now rather than after PINGs go
    /// unanswered.
    pub fn network_changed(&self) {
        self.repair.store(true, Ordering::Release);
    }

    /// Stops the worker and closes the TUN; blocks for up to a second or two. Calling it again
    /// does nothing.
    pub fn stop(&self) {
        self.stop.cancel();
        let mut runtime = self.runtime.lock().unwrap();
        if let (Some(mut worker), Some(runtime)) =
            (self.worker.lock().unwrap().take(), runtime.as_ref())
        {
            runtime.block_on(async {
                // The worker releases its leases over HTTP; past this, abandon a stalled call.
                if tokio::time::timeout(Duration::from_millis(500), &mut worker)
                    .await
                    .is_err()
                {
                    self.provider.cancel_control();
                    let _ = worker.await;
                }
            });
        }
        if let Some(runtime) = runtime.take() {
            // Dropping the shim's tasks closes the TUN.
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
        self.status.lock().unwrap().state = Some(State::Stopped);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Provider {
    platform: Arc<dyn Platform>,
    repair: Arc<AtomicBool>,
    stopping: AtomicBool,
    control: Mutex<Option<std::net::TcpStream>>,
}
impl Provider {
    fn cancel_control(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(socket) = self.control.lock().unwrap().take() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
}
impl SocketProvider for Provider {
    fn control_socket(&self, peer: SocketAddr) -> io::Result<std::net::TcpStream> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "transport stopping",
            ));
        }
        let socket = socket2::Socket::new(
            socket2::Domain::for_address(peer),
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        self.platform.protect_socket(socket.as_raw_fd())?;
        {
            let mut current = self.control.lock().unwrap();
            if self.stopping.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "transport stopping",
                ));
            }
            *current = Some(socket.try_clone()?.into());
        }
        socket.connect_timeout(&peer.into(), Duration::from_secs(3))?;
        Ok(socket.into())
    }
    fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket> {
        let socket = match peer {
            SocketAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?,
            SocketAddr::V6(_) => UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))?,
        };
        self.platform.protect_socket(socket.as_raw_fd())?;
        Ok(socket)
    }
    fn prepare_port(&self, _: SocketAddr) -> io::Result<UdpSocket> {
        Err(io::Error::other("client only"))
    }
    fn commit_port(&self, _: u16) -> io::Result<()> {
        Err(io::Error::other("client only"))
    }
    fn retire_port(&self, _: u16) -> io::Result<()> {
        Err(io::Error::other("client only"))
    }
    fn reconcile_ports(&self) -> io::Result<()> {
        Ok(())
    }
    fn repair_requested(&self) -> bool {
        self.repair.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_and_udp_are_protected_before_connect_and_stop_closes_control() {
        use std::io::Read;
        use std::os::fd::BorrowedFd;
        use std::sync::atomic::AtomicUsize;
        struct Protection(AtomicUsize);
        impl Platform for Protection {
            fn protect_socket(&self, fd: RawFd) -> io::Result<()> {
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
                assert!(socket2::SockRef::from(&borrowed).peer_addr().is_err());
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
        let protection = Arc::new(Protection(AtomicUsize::new(0)));
        let provider = Provider {
            platform: protection.clone(),
            repair: Arc::new(AtomicBool::new(false)),
            stopping: AtomicBool::new(false),
            control: Mutex::new(None),
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = listener.local_addr().unwrap();
        let _tcp = provider.control_socket(peer).unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        accepted
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let _udp = provider.client_socket(peer).unwrap();
        assert_eq!(protection.0.load(Ordering::Relaxed), 2);
        provider.cancel_control();
        assert_eq!(accepted.read(&mut [0]).unwrap(), 0);
        assert_eq!(
            provider.control_socket(peer).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn routes_cover_the_dns_address_and_not_private_networks() {
        let routes = tunnel_routes();
        let covers = |ip: Ipv4Addr| routes.iter().any(|r| r.contains(&ip));
        assert!(covers(DNS_ADDRESS));
        assert!(covers(Ipv4Addr::new(8, 8, 8, 8)));
        for private in [
            [10, 0, 0, 1],
            [192, 168, 1, 1],
            [172, 16, 0, 1],
            [127, 0, 0, 1],
        ] {
            assert!(!covers(private.into()));
        }
        assert!(
            !covers(Ipv4Addr::new(223, 5, 5, 5)),
            "a local-region resolver"
        );
        assert!(routes.len() < 2500, "{} routes", routes.len());
    }

    #[test]
    fn worker_events_set_the_state() {
        let mut status = WorkerStatus::default();
        for (event, state) in [
            ("degraded", State::Degraded),
            ("path_alive", State::Connected),
            ("repairing", State::Repairing),
            ("repaired", State::Connected),
        ] {
            status.observe(&serde_json::json!({"event": event}));
            assert_eq!(status.state, Some(state));
        }
        status.observe(&serde_json::json!({"event":"stats","active_repairs":2,"fallbacks":1}));
        assert_eq!((status.repairs, status.fallbacks), (2, 1));
        assert_eq!(status.state, Some(State::Connected));
        assert_eq!(status.path, None);
        let path = PathCounters {
            wakes: 7,
            device_queue_high: 3,
            ..Default::default()
        };
        status.observe(&serde_json::json!({"event":"stats","path":path}));
        assert_eq!(status.path, Some(path));
    }
}
