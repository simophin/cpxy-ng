//! Portable Unix client: asynchronous HTTP preparation, acknowledged handover and repair.
//!
//! [`prepare_path`] validates a first UDP path synchronously, before the caller has a TUN.
//! [`run`]/[`run_prepared`] then drive every allocation from one Tokio task: it wakes for a
//! device packet, a readable UDP socket, a finished HTTP job, cancellation or the timer tick,
//! and each wake makes one bounded pass over all of its state.
use super::http_client::{Client, Pending};
use crate::{
    device::Device,
    v2::{
        codec::{Control, Direction, Packet, Rx, Tx, MAX_FRAME},
        http::{random, unhex},
        state::{Health, HealthAction},
    },
    worker::{Events, SocketProvider},
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    future::poll_fn,
    io,
    net::SocketAddr,
    sync::Arc,
    task::Poll,
    thread,
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::mpsc::error::{TryRecvError, TrySendError},
    task::{self, JoinHandle},
    time,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Host-independent client policy; TUN, socket protection and identity are supplied
/// by the caller. No firewall, route mutation or platform lifecycle code here.
pub struct ClientConfig {
    pub entry: SocketAddr,
    pub inner: std::net::Ipv4Addr,
    pub keepalive_seconds: u64,
}

/// Wake at least this often. Protocol timers are 500 ms or longer, and the provider's repair
/// request is polled, so nothing waits on a finer deadline.
const TICK: Duration = Duration::from_millis(250);
/// Device packets and datagrams per socket handled in one pass before other work gets a turn.
const BUDGET: usize = 64;

/// Cumulative packet-path counts, reported as `path` in each `stats` event. Aggregates only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PathCounters {
    /// Worker wakes, and those from the timer tick alone.
    pub wakes: u64,
    pub timer_wakes: u64,
    /// Packets taken from the device, and those with no active allocation or no frame to seal
    /// them into.
    pub device_reads: u64,
    pub outbound_unrouted: u64,
    /// Packets queued to the device, dropped on its full queue, and the queue's high-water mark.
    pub device_writes: u64,
    pub device_queue_full: u64,
    pub device_queue_high: u64,
    /// Passes that stopped receiving because the device queue was full.
    pub device_backpressure: u64,
    /// Datagrams received, those from another source, and those failing authentication/replay.
    pub udp_recvs: u64,
    pub udp_foreign: u64,
    pub udp_rejected: u64,
    /// Data frames sent, and those dropped because the send failed.
    pub udp_sends: u64,
    pub udp_send_failed: u64,
}

fn invalid() -> io::Error {
    io::Error::other("invalid UDP allocation response")
}
fn device_closed() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "packet device closed")
}
fn milliseconds(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
fn ticks(now: u64) -> u32 {
    (now as u32).wrapping_mul(90)
}
fn idle_delay(seconds: u64) -> u64 {
    use rand::Rng;
    rand::rngs::OsRng.gen_range(seconds * 800..=seconds * 1200)
}
fn release_params(id: Uuid) -> serde_json::Map<String, Value> {
    json!({"allocation_id": id}).as_object().unwrap().clone()
}

/// A validated `create_udp` result.
struct Lease {
    id: Uuid,
    generation: String,
    secret: [u8; 32],
    peer: SocketAddr,
    activation_floor: u64,
}
impl Lease {
    fn parse(result: &Value, entry: SocketAddr) -> io::Result<Self> {
        let id: Uuid =
            serde_json::from_value(result["allocation_id"].clone()).map_err(|_| invalid())?;
        let secret =
            unhex::<32>(result["secret"].as_str().ok_or_else(invalid)?).ok_or_else(invalid)?;
        let generation = result["generation"]
            .as_str()
            .ok_or_else(invalid)?
            .to_owned();
        if unhex::<16>(&generation).is_none() {
            return Err(invalid());
        }
        let port: u16 = result["port"]
            .as_u64()
            .and_then(|p| p.try_into().ok())
            .filter(|p| *p != 0)
            .ok_or_else(invalid)?;
        for (name, value) in [
            ("mtu", 1200),
            ("startup_ms", 30000),
            ("inactivity_ms", 120000),
            ("rotation_ms", 300000),
            ("lead_ms", 10000),
            ("drain_ms", 10000),
        ] {
            if result[name] != value {
                return Err(invalid());
            }
        }
        let mut peer = entry;
        peer.set_port(port);
        Ok(Self {
            id,
            generation,
            secret,
            peer,
            activation_floor: result["activation_floor"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(invalid)?,
        })
    }
    fn keys(&self, inner: std::net::Ipv4Addr) -> (Tx, Rx) {
        let id = *self.id.as_bytes();
        (
            Tx::new(&self.secret, id, Direction::Upstream, inner),
            Rx::new(&self.secret, id, Direction::Downstream, inner),
        )
    }
}

/// A protected, buffered, nonblocking client socket toward `peer`.
fn client_socket(
    provider: &dyn SocketProvider,
    peer: SocketAddr,
) -> io::Result<std::net::UdpSocket> {
    let socket = provider.client_socket(peer)?;
    crate::worker::buffer_transport(&socket)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

#[derive(Clone, Copy)]
enum Phase {
    Open,
    Response([u8; 16]),
    Ready,
    Activating(u64),
    Established,
}
struct Allocation {
    id: Uuid,
    generation: String,
    peer: SocketAddr,
    socket: UdpSocket,
    candidate: Option<UdpSocket>,
    old_socket: Option<(u64, UdpSocket)>,
    tx: Tx,
    rx: Rx,
    phase: Phase,
    attempt: u64,
    token: [u8; 16],
    retry: u64,
    started: u64,
    activated: Option<u64>,
    retire: Option<u64>,
    health: Health,
    repair_after: u64,
    activate_early: bool,
    activation_floor: u64,
}
impl Allocation {
    fn new(
        lease: Lease,
        socket: std::net::UdpSocket,
        (tx, rx): (Tx, Rx),
        now: u64,
        keepalive: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            id: lease.id,
            generation: lease.generation,
            peer: lease.peer,
            socket: UdpSocket::from_std(socket)?,
            candidate: None,
            old_socket: None,
            tx,
            rx,
            phase: Phase::Open,
            attempt: 1,
            token: random(),
            retry: 0,
            started: now,
            activated: None,
            retire: None,
            health: Health::new(now, idle_delay(keepalive)),
            repair_after: 0,
            activate_early: false,
            activation_floor: lease.activation_floor,
        })
    }
    /// The current, candidate and draining sockets: all receive, sharing one replay window.
    fn sockets(&self) -> impl Iterator<Item = &UdpSocket> {
        std::iter::once(&self.socket)
            .chain(self.candidate.iter())
            .chain(self.old_socket.iter().map(|(_, s)| s))
    }
    fn handshake_socket(&self) -> &UdpSocket {
        self.candidate.as_ref().unwrap_or(&self.socket)
    }
    fn send(&mut self, control: Control, now: u64, candidate: bool, keepalive: u64) {
        if let Some(frame) = self.tx.control(control, ticks(now)) {
            let socket = if candidate {
                self.handshake_socket()
            } else {
                &self.socket
            };
            if socket.try_send_to(&frame, self.peer).is_ok() {
                self.health.sent(now, idle_delay(keepalive));
            }
        } else {
            self.retire = Some(now);
        }
    }
    fn repair(&mut self, provider: &dyn SocketProvider, now: u64) -> io::Result<()> {
        if self.candidate.is_some() || now < self.repair_after {
            return Ok(());
        }
        self.candidate = Some(UdpSocket::from_std(client_socket(provider, self.peer)?)?);
        self.attempt = self.attempt.checked_add(1).ok_or_else(invalid)?;
        self.token = random();
        self.phase = Phase::Open;
        self.retry = 0;
        self.started = now;
        self.repair_after = now + 30_000;
        Ok(())
    }
}

enum JobResult {
    Create(Value),
    Released,
}
type Job = JoinHandle<(Client, io::Result<JobResult>)>;
/// Preserve an unknown create outcome across reconnects. Authenticated terminal
/// errors permit a new operation; timeouts never silently create another lease.
fn create_job(mut client: Client, inner: std::net::Ipv4Addr, stop: CancellationToken) -> Job {
    task::spawn_blocking(move || {
        let mut pending: Option<Pending> = None;
        let mut delay = 1000;
        // A release or another HTTP job may already have consumed a generation transition.
        // Check every existing session before recreation/rotation, while allowing a fresh
        // manually-addressed Linux caller to claim its configured address first.
        let mut verify_mapping = client.has_session();
        loop {
            if stop.is_cancelled() {
                return (
                    client,
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "transport stopped",
                    )),
                );
            }
            let result = (|| {
                if verify_mapping {
                    let assigned = client.call("allocate_ip", Default::default())?;
                    let assigned: std::net::Ipv4Addr =
                        serde_json::from_value(assigned["inner"].clone()).map_err(|_| invalid())?;
                    if assigned != inner {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrNotAvailable,
                            "assigned tunnel address changed",
                        ));
                    }
                    verify_mapping = false;
                }
                if pending.is_none() {
                    pending = Some(client.operation(
                        "create_udp",
                        json!({"inner":inner}).as_object().unwrap().clone(),
                    )?);
                }
                client.execute(pending.as_ref().unwrap())
            })();
            match result {
                Ok(value) => return (client, Ok(JobResult::Create(value))),
                Err(e) => {
                    if e.kind() == io::ErrorKind::AddrNotAvailable {
                        return (client, Err(e));
                    }
                    if e.kind() == io::ErrorKind::ConnectionReset {
                        verify_mapping = true;
                    }
                    // These responses are authenticated known outcomes with no
                    // recoverable live create. A new operation may be attempted.
                    if e.kind() == io::ErrorKind::ConnectionReset
                        || [
                            "-32007", "-32008", "-32009", "-32010", "-32011", "-32012", "-32013",
                        ]
                        .iter()
                        .any(|code| e.to_string() == format!("authenticated control error {code}"))
                    {
                        pending = None;
                    }
                    let until = Instant::now() + Duration::from_millis(idle_delay(delay) / 1000);
                    while !stop.is_cancelled() && Instant::now() < until {
                        thread::sleep(Duration::from_millis(20));
                    }
                    delay = (delay * 2).min(30_000);
                }
            }
        }
    })
}
fn release_job(mut client: Client, id: Uuid) -> Job {
    task::spawn_blocking(move || {
        let result = client
            .call("release_udp", release_params(id))
            .map(|_| JobResult::Released);
        (client, result)
    })
}

/// A path that completed the authenticated UDP challenge, before its keys and socket are
/// adopted by the worker.
struct Validated {
    lease: Lease,
    socket: std::net::UdpSocket,
    keys: (Tx, Rx),
    attempt: u64,
    token: [u8; 16],
}

/// An authenticated, bidirectional UDP path, not yet activated. Racing candidates
/// must not steal the active route from each other. The winner is transferred to
/// the packet worker with the same keys, socket and counters; losers release their leases.
pub struct PreparedPath {
    control: Option<Client>,
    path: Option<Validated>,
    start: Instant,
    entry: SocketAddr,
    inner: std::net::Ipv4Addr,
}
impl PreparedPath {
    pub fn entry(&self) -> SocketAddr {
        self.entry
    }
    pub fn inner(&self) -> std::net::Ipv4Addr {
        self.inner
    }
}
impl Drop for PreparedPath {
    fn drop(&mut self) {
        if let (Some(client), Some(path)) = (&mut self.control, &self.path) {
            let _ = client.call("release_udp", release_params(path.lease.id));
        }
    }
}

/// Complete HTTP setup and the authenticated UDP challenge before considering an
/// address usable. Cancellation never activates a losing candidate. Blocking: this runs
/// before the caller has a runtime, on a racing thread per candidate.
pub fn prepare_path(
    mut control: Client,
    provider: &dyn SocketProvider,
    cancelled: &std::sync::atomic::AtomicBool,
    keepalive: u64,
) -> io::Result<PreparedPath> {
    use std::sync::atomic::Ordering;
    if !(5..=45).contains(&keepalive) {
        return Err(io::Error::other("invalid keepalive interval"));
    }
    let start = Instant::now();
    let result = control.call("allocate_ip", Default::default())?;
    let inner = serde_json::from_value(result["inner"].clone()).map_err(|_| invalid())?;
    if cancelled.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let value = control.call(
        "create_udp",
        json!({"inner":inner}).as_object().unwrap().clone(),
    )?;
    let setup = Lease::parse(&value, control.peer).and_then(|lease| {
        let socket = client_socket(provider, lease.peer)?;
        socket.set_nonblocking(false)?;
        let keys = lease.keys(inner);
        Ok((lease, socket, keys))
    });
    let (lease, socket, keys) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            if let Some(id) = value["allocation_id"].as_str() {
                let _ = control.call(
                    "release_udp",
                    json!({"allocation_id":id}).as_object().unwrap().clone(),
                );
            }
            return Err(error);
        }
    };
    let mut prepared = PreparedPath {
        entry: control.peer,
        inner,
        control: Some(control),
        path: Some(Validated {
            lease,
            socket,
            keys,
            attempt: 1,
            token: random(),
        }),
        start,
    };
    let path = prepared.path.as_mut().unwrap();
    let (tx, rx) = &mut path.keys;
    let mut challenge = None;
    let mut wire = [0; MAX_FRAME + 1];
    let deadline = Instant::now() + Duration::from_secs(7);
    let mut next_send = Instant::now();
    while !cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
        if Instant::now() >= next_send {
            let control = match challenge {
                Some(challenge) => Control::PathResponse {
                    attempt: path.attempt,
                    token: path.token,
                    challenge,
                },
                None => Control::PathOpen {
                    attempt: path.attempt,
                    token: path.token,
                },
            };
            let frame = tx
                .control(control, ticks(milliseconds(start)))
                .ok_or_else(invalid)?;
            let _ = path.socket.send_to(&frame, path.lease.peer);
            next_send = Instant::now() + Duration::from_millis(500);
        }
        // Short reads keep cancellation of a losing candidate prompt.
        let wait = next_send
            .min(deadline)
            .min(Instant::now() + Duration::from_millis(50))
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        path.socket.set_read_timeout(Some(wait))?;
        match path.socket.recv_from(&mut wire) {
            Ok((n, peer)) if peer == path.lease.peer => match rx.open(&wire[..n]) {
                Some(Packet::Control(Control::PathChallenge {
                    attempt,
                    token,
                    challenge: c,
                })) if attempt == path.attempt && token == path.token => {
                    challenge = Some(c);
                    next_send = Instant::now();
                }
                Some(Packet::Control(Control::PathReady { attempt, token }))
                    if attempt == path.attempt && token == path.token =>
                {
                    path.socket.set_read_timeout(None)?;
                    path.socket.set_nonblocking(true)?;
                    return Ok(prepared);
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        if cancelled.load(Ordering::Acquire) {
            io::ErrorKind::Interrupted
        } else {
            io::ErrorKind::TimedOut
        },
        "UDP candidate path did not become ready",
    ))
}

/// Start the winning path without creating a second allocation or reusing counters.
/// Ends when `stop` is cancelled or the device closes, after releasing its leases.
pub async fn run_prepared(
    settings: ClientConfig,
    mut prepared: PreparedPath,
    device: Device,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    stop: CancellationToken,
) -> io::Result<()> {
    if settings.entry != prepared.entry || settings.inner != prepared.inner {
        return Err(io::Error::other(
            "prepared path does not match client settings",
        ));
    }
    let mut worker = Worker::new(
        settings,
        prepared.start,
        prepared.control.take().unwrap(),
        device,
        provider,
        events,
        stop,
    )?;
    let path = prepared.path.take().unwrap();
    let mut a = Allocation::new(path.lease, path.socket, path.keys, 0, worker.keepalive)?;
    (a.attempt, a.token, a.phase) = (path.attempt, path.token, Phase::Ready);
    worker.allocations.push(a);
    worker.run().await
}

/// Create and validate a path, then carry traffic as [`run_prepared`] does.
pub async fn run(
    settings: ClientConfig,
    control: Client,
    device: Device,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    stop: CancellationToken,
) -> io::Result<()> {
    Worker::new(
        settings,
        Instant::now(),
        control,
        device,
        provider,
        events,
        stop,
    )?
    .run()
    .await
}

enum Wake {
    Stop,
    Job(Box<Result<(Client, io::Result<JobResult>), task::JoinError>>),
    Packet(Option<Vec<u8>>),
    Socket,
    Tick,
}

/// Resolves once any allocation socket, including draining ones, has a datagram to receive.
async fn readable(allocations: &[Allocation]) {
    poll_fn(|cx| {
        let mut sockets = allocations.iter().flat_map(Allocation::sockets);
        if sockets.any(|s| s.poll_recv_ready(cx).is_ready()) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

struct Worker {
    entry: SocketAddr,
    inner: std::net::Ipv4Addr,
    keepalive: u64,
    start: Instant,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    /// The caller's token, or this worker's own failure: either ends it.
    stop: CancellationToken,
    device: Device,
    allocations: Vec<Allocation>,
    active: Option<Uuid>,
    activation: u64,
    /// The control client, lent to the running job if any.
    http: Option<Client>,
    job: Option<Job>,
    releases: VecDeque<Uuid>,
    next_prepare: u64,
    recreate: bool,
    stats_at: u64,
    ipv6_drops: u64,
    counters: PathCounters,
    /// The device queue is full: leave datagrams in the socket buffers until it drains,
    /// rather than decrypting packets only to drop them.
    backpressure: bool,
    wire: Box<[u8; MAX_FRAME + 1]>,
}

impl Worker {
    fn new(
        settings: ClientConfig,
        start: Instant,
        control: Client,
        device: Device,
        provider: Arc<dyn SocketProvider>,
        events: Events,
        stop: CancellationToken,
    ) -> io::Result<Self> {
        if !(5..=45).contains(&settings.keepalive_seconds) {
            return Err(io::Error::other("invalid keepalive interval"));
        }
        provider.packet_ready()?;
        Ok(Self {
            entry: settings.entry,
            inner: settings.inner,
            keepalive: settings.keepalive_seconds,
            start,
            provider,
            events,
            stop: stop.child_token(),
            device,
            allocations: Vec::new(),
            active: None,
            activation: 0,
            http: Some(control),
            job: None,
            releases: VecDeque::new(),
            next_prepare: 0,
            recreate: false,
            stats_at: 10_000,
            ipv6_drops: 0,
            counters: PathCounters::default(),
            backpressure: false,
            wire: Box::new([0; MAX_FRAME + 1]),
        })
    }

    async fn run(mut self) -> io::Result<()> {
        let result = self.serve().await;
        self.stop.cancel();
        let shutdown = self.shutdown().await;
        result.and(shutdown)
    }

    async fn serve(&mut self) -> io::Result<()> {
        loop {
            let wake = self.wait().await;
            self.counters.wakes += 1;
            let now = milliseconds(self.start);
            match wake {
                Wake::Stop => return Ok(()),
                Wake::Job(result) => {
                    self.job = None;
                    let (client, result) =
                        (*result).map_err(|_| io::Error::other("HTTP preparation panicked"))?;
                    self.http = Some(client);
                    self.finish_job(result, now)?;
                }
                Wake::Packet(Some(packet)) => self.outbound(&packet, now),
                Wake::Packet(None) => return Err(device_closed()),
                Wake::Socket => {}
                Wake::Tick => self.counters.timer_wakes += 1,
            }
            self.service(now)?;
        }
    }

    async fn wait(&mut self) -> Wake {
        let Self {
            stop,
            job,
            device,
            allocations,
            backpressure,
            ..
        } = self;
        let (has_job, backpressure) = (job.is_some(), *backpressure);
        tokio::select! {
            () = stop.cancelled() => Wake::Stop,
            result = async { job.as_mut().unwrap().await }, if has_job => Wake::Job(Box::new(result)),
            packet = device.reads.recv() => Wake::Packet(packet),
            () = readable(allocations), if !backpressure => Wake::Socket,
            permit = device.writes.reserve(), if backpressure => match permit {
                Ok(_) => Wake::Socket,
                Err(_) => Wake::Packet(None),
            },
            () = time::sleep(TICK) => Wake::Tick,
        }
    }

    /// After cancellation or failure: finish any HTTP job, then release every lease.
    async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(job) = self.job.take() {
            let (client, created) = job
                .await
                .map_err(|_| io::Error::other("HTTP owner panicked"))?;
            self.http = Some(client);
            if let Ok(JobResult::Create(value)) = created {
                if let Ok(id) = serde_json::from_value(value["allocation_id"].clone()) {
                    self.releases.push_back(id);
                }
            }
        }
        let Some(mut client) = self.http.take() else {
            return Ok(());
        };
        let ids: Vec<_> = self
            .allocations
            .drain(..)
            .map(|a| a.id)
            .chain(self.releases.drain(..))
            .collect();
        task::spawn_blocking(move || {
            for id in ids {
                let _ = client.call("release_udp", release_params(id));
            }
        })
        .await
        .map_err(|_| io::Error::other("HTTP owner panicked"))
    }

    fn finish_job(&mut self, result: io::Result<JobResult>, now: u64) -> io::Result<()> {
        // The platform must prepare a replacement TUN using the newly assigned address.
        if result
            .as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::AddrNotAvailable)
        {
            return Err(result.err().unwrap());
        }
        let Ok(JobResult::Create(value)) = result else {
            return Ok(());
        };
        let allocation = Lease::parse(&value, self.entry).and_then(|lease| {
            let socket = client_socket(self.provider.as_ref(), lease.peer)?;
            let keys = lease.keys(self.inner);
            Allocation::new(lease, socket, keys, now, self.keepalive)
        });
        match allocation {
            Ok(mut a) => {
                a.activate_early = self.recreate;
                if self
                    .allocations
                    .first()
                    .is_some_and(|old| old.generation != a.generation)
                {
                    // The endpoint restarted: its old leases and routes are gone.
                    self.allocations.clear();
                    self.active = None;
                }
                self.allocations.push(a);
                self.recreate = false;
            }
            Err(_) => {
                if let Ok(id) = serde_json::from_value(value["allocation_id"].clone()) {
                    self.releases.push_back(id);
                }
                self.next_prepare = now + 1000;
            }
        }
        Ok(())
    }

    /// One device packet toward the endpoint, sealed for the active allocation.
    fn outbound(&mut self, packet: &[u8], now: u64) {
        self.counters.device_reads += 1;
        if packet.first().is_some_and(|b| b >> 4 == 6) {
            self.ipv6_drops += 1;
            return;
        }
        if packet.len() > crate::MTU {
            return;
        }
        let active = self.active;
        let sealed = active
            .and_then(|id| self.allocations.iter_mut().find(|a| a.id == id))
            .and_then(|a| {
                let frame = a.tx.data(packet, ticks(now))?;
                Some((a, frame))
            });
        let Some((a, frame)) = sealed else {
            self.counters.outbound_unrouted += 1;
            return;
        };
        self.counters.udp_sends += 1;
        if a.socket.try_send_to(&frame, a.peer).is_ok() {
            a.health.sent(now, idle_delay(self.keepalive));
        } else {
            self.counters.udp_send_failed += 1;
        }
    }

    /// One packet from the endpoint toward the device. Never waits: a full queue drops.
    fn deliver(device: &Device, counters: &mut PathCounters, packet: Vec<u8>) -> io::Result<()> {
        match device.writes.try_send(packet) {
            Ok(()) => {
                counters.device_writes += 1;
                let queued = device.writes.max_capacity() - device.writes.capacity();
                counters.device_queue_high = counters.device_queue_high.max(queued as u64);
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                counters.device_queue_full += 1;
                Ok(())
            }
            Err(TrySendError::Closed(_)) => Err(device_closed()),
        }
    }

    /// Everything that does not wait: device and socket input, HTTP jobs and timers.
    fn service(&mut self, now: u64) -> io::Result<()> {
        if now >= self.stats_at {
            (self.events)(json!({
                "event": "stats",
                "protocol": "rtp-http-v2",
                "inner_ipv6_drops": self.ipv6_drops,
                "path": self.counters,
            }));
            self.stats_at = now.saturating_add(10_000);
        }
        for _ in 0..BUDGET {
            match self.device.reads.try_recv() {
                Ok(packet) => self.outbound(&packet, now),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(device_closed()),
            }
        }
        self.start_job(now);
        let rotation_due = self
            .active
            .and_then(|id| self.allocations.iter().find(|a| a.id == id))
            .and_then(|a| a.activated)
            .map_or(0, |at| at + 300_000);
        let mut acknowledged = None;
        let keepalive = self.keepalive;
        self.backpressure = false;
        for a in &mut self.allocations {
            if a.retire.is_some_and(|at| now >= at) {
                continue;
            }
            if a.old_socket.as_ref().is_some_and(|(at, _)| now >= *at) {
                a.old_socket = None;
            }
            // Borrow the sockets field by field: the replay window in `a.rx` is updated
            // while they are read.
            let mut inbound = Vec::new();
            for socket in std::iter::once(&a.socket)
                .chain(a.candidate.iter())
                .chain(a.old_socket.iter().map(|(_, s)| s))
            {
                for _ in 0..BUDGET {
                    // Each datagram may need a device slot, including those not yet delivered.
                    if self.device.writes.capacity() <= inbound.len() {
                        self.backpressure = true;
                        break;
                    }
                    let Ok((n, peer)) = socket.try_recv_from(&mut self.wire[..]) else {
                        break;
                    };
                    self.counters.udp_recvs += 1;
                    if peer != a.peer {
                        self.counters.udp_foreign += 1;
                    } else if let Some(packet) = a.rx.open(&self.wire[..n]) {
                        inbound.push(packet);
                    } else {
                        self.counters.udp_rejected += 1;
                    }
                }
            }
            for received in inbound {
                match received {
                    Packet::Data(ip) => {
                        a.health.received(now, None);
                        Self::deliver(&self.device, &mut self.counters, ip)?;
                    }
                    Packet::Control(c) => {
                        let pong = match c {
                            Control::Pong(token) => Some(token),
                            _ => None,
                        };
                        a.health.received(now, pong);
                        match c {
                            Control::PathChallenge {
                                attempt,
                                token,
                                challenge,
                            } if attempt == a.attempt
                                && token == a.token
                                && matches!(a.phase, Phase::Open | Phase::Response(_)) =>
                            {
                                a.phase = Phase::Response(challenge);
                                a.retry = 0;
                            }
                            Control::PathReady { attempt, token }
                                if attempt == a.attempt
                                    && token == a.token
                                    && matches!(a.phase, Phase::Open | Phase::Response(_)) =>
                            {
                                a.phase = Phase::Ready;
                                a.retry = 0;
                            }
                            Control::Activated {
                                activation: ack,
                                attempt,
                                token,
                            } if attempt == a.attempt
                                && token == a.token
                                && matches!(a.phase, Phase::Activating(n) if n == ack) =>
                            {
                                if let Some(socket) = a.candidate.take() {
                                    let old = std::mem::replace(&mut a.socket, socket);
                                    a.old_socket = Some((now + 10_000, old));
                                }
                                a.phase = Phase::Established;
                                a.activated = Some(now);
                                a.health = Health::new(now, idle_delay(keepalive));
                                acknowledged = Some(a.id);
                            }
                            _ => {}
                        }
                    }
                }
            }
            if matches!(a.phase, Phase::Ready)
                && (a.candidate.is_some()
                    || a.activate_early
                    || self.active.is_none()
                    || now >= rotation_due)
            {
                self.activation = self
                    .activation
                    .max(a.activation_floor - 1)
                    .checked_add(1)
                    .ok_or_else(invalid)?;
                a.phase = Phase::Activating(self.activation);
                a.started = now;
                a.retry = 0;
            }
            if now >= a.retry {
                let control = match a.phase {
                    Phase::Open | Phase::Ready => Some(Control::PathOpen {
                        attempt: a.attempt,
                        token: a.token,
                    }),
                    Phase::Response(challenge) => Some(Control::PathResponse {
                        attempt: a.attempt,
                        token: a.token,
                        challenge,
                    }),
                    Phase::Activating(n) => Some(Control::Activate {
                        activation: n,
                        attempt: a.attempt,
                        token: a.token,
                    }),
                    Phase::Established => None,
                };
                if let Some(c) = control {
                    a.send(c, now, true, keepalive);
                    a.retry = now + 500;
                }
            }
            if a.activated.is_some() && a.retire.is_none() {
                if self.provider.repair_requested() {
                    let _ = a.repair(self.provider.as_ref(), now);
                }
                match a.health.poll(now, random()) {
                    Some(HealthAction::Ping(token)) => {
                        a.send(Control::Ping(token), now, false, keepalive)
                    }
                    Some(HealthAction::Suspect) => {
                        (self.events)(json!({"event":"repairing"}));
                        if a.repair(self.provider.as_ref(), now).is_err() || a.candidate.is_none() {
                            self.recreate = true;
                        }
                    }
                    None => {}
                }
            }
            if now.saturating_sub(a.started) >= 7000
                && matches!(
                    a.phase,
                    Phase::Open | Phase::Response(_) | Phase::Activating(_)
                )
            {
                if a.activated.is_some() {
                    a.candidate = None;
                    a.phase = Phase::Established;
                    self.recreate = true;
                    (self.events)(json!({"event":"degraded"}));
                } else {
                    a.retire = Some(now);
                    self.next_prepare = now + 1000;
                }
            }
        }
        if self.backpressure {
            self.counters.device_backpressure += 1;
        }
        if let Some(id) = acknowledged {
            for a in &mut self.allocations {
                if a.id != id && a.retire.is_none() {
                    a.retire = Some(now + 10_000);
                }
            }
            self.active = Some(id);
            (self.events)(json!({"event":"connected","protocol":"rtp-http-v2"}));
        }
        let releases = &mut self.releases;
        self.allocations.retain(|a| {
            if a.retire.is_some_and(|at| now >= at) {
                releases.push_back(a.id);
                false
            } else {
                true
            }
        });
        Ok(())
    }

    /// Lend the control client to a release, or to a create when a new path is due.
    fn start_job(&mut self, now: u64) {
        if self.job.is_some() {
            return;
        }
        if let Some(id) = self.releases.pop_front() {
            self.job = Some(release_job(self.http.take().unwrap(), id));
            return;
        }
        let due = self.allocations.is_empty()
            || self.recreate
            || (self.allocations.len() == 1
                && self.allocations[0]
                    .activated
                    .is_some_and(|at| now >= at + 290_000));
        if now >= self.next_prepare && due && self.allocations.len() < 2 {
            self.job = Some(create_job(
                self.http.take().unwrap(),
                self.inner,
                self.stop.clone(),
            ));
            self.next_prepare = now + 1000;
        }
    }
}
