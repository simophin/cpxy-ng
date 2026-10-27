//! Portable Unix client: asynchronous HTTP preparation, acknowledged handover and repair.
use super::http_client::{Client, Pending};
use crate::{
    v2::{
        codec::{Control, Direction, MAX_FRAME, Packet, Rx, Tx},
        http::{random, unhex},
        state::{Health, HealthAction},
    },
    worker::{Events, PacketIo, SocketProvider},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Host-independent client policy; TUN, socket protection and identity are supplied
/// by the caller. No firewall, route mutation or platform lifecycle code here.
pub struct ClientConfig {
    pub entry: SocketAddr,
    pub inner: std::net::Ipv4Addr,
    pub keepalive_seconds: u64,
}

fn invalid() -> io::Error {
    io::Error::other("invalid UDP allocation response")
}
fn milliseconds(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
fn idle_delay(seconds: u64) -> u64 {
    use rand::Rng;
    rand::rngs::OsRng.gen_range(seconds * 800..=seconds * 1200)
}
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
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
        result: Value,
        entry: SocketAddr,
        inner: std::net::Ipv4Addr,
        provider: &dyn SocketProvider,
        now: u64,
        keepalive: u64,
    ) -> io::Result<Self> {
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
        let socket = provider.client_socket(peer)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            id,
            generation,
            peer,
            socket,
            candidate: None,
            old_socket: None,
            tx: Tx::new(&secret, *id.as_bytes(), Direction::Upstream, inner),
            rx: Rx::new(&secret, *id.as_bytes(), Direction::Downstream, inner),
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
            activation_floor: result["activation_floor"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(invalid)?,
        })
    }
    fn handshake_socket(&self) -> &UdpSocket {
        self.candidate.as_ref().unwrap_or(&self.socket)
    }
    fn send(&mut self, control: Control, now: u64, candidate: bool, keepalive: u64) {
        if let Some(frame) = self.tx.control(control, (now as u32).wrapping_mul(90)) {
            let socket = if candidate {
                self.handshake_socket()
            } else {
                &self.socket
            };
            if socket.send_to(&frame, self.peer).is_ok() {
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
        let socket = provider.client_socket(self.peer)?;
        socket.set_nonblocking(true)?;
        self.candidate = Some(socket);
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
type Job = thread::JoinHandle<(Client, io::Result<JobResult>)>;
/// Preserve an unknown create outcome across reconnects. Authenticated terminal
/// errors permit a new operation; timeouts never silently create another lease.
fn create_job(mut client: Client, inner: std::net::Ipv4Addr, running: Arc<AtomicBool>) -> Job {
    thread::spawn(move || {
        let mut pending: Option<Pending> = None;
        let mut delay = 1000;
        let mut verify_mapping = false;
        loop {
            if !running.load(Ordering::Relaxed) {
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
                    let wait = idle_delay(delay) / 1000;
                    let until = Instant::now() + Duration::from_millis(wait);
                    while running.load(Ordering::Relaxed) && Instant::now() < until {
                        thread::sleep(Duration::from_millis(20));
                    }
                    delay = (delay * 2).min(30_000);
                }
            }
        }
    })
}
fn release_job(mut client: Client, id: Uuid) -> Job {
    thread::spawn(move || {
        let result = client
            .call(
                "release_udp",
                json!({"allocation_id":id}).as_object().unwrap().clone(),
            )
            .map(|_| JobResult::Released);
        (client, result)
    })
}

/// An authenticated, bidirectional UDP path, not yet activated. Racing candidates
/// must not steal the active route from each other. The winner is transferred to
/// the packet worker with the same keys, socket and counters; losers release their leases.
pub struct PreparedPath {
    control: Option<Client>,
    allocation: Option<Allocation>,
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
        if let (Some(client), Some(allocation)) = (&mut self.control, &self.allocation) {
            let _ = client.call(
                "release_udp",
                json!({"allocation_id": allocation.id})
                    .as_object()
                    .unwrap()
                    .clone(),
            );
        }
    }
}

/// Complete HTTP setup and the authenticated UDP challenge before considering an
/// address usable. Cancellation never activates a losing candidate.
pub fn prepare_path(
    mut control: Client,
    provider: &dyn SocketProvider,
    cancelled: &AtomicBool,
    keepalive: u64,
) -> io::Result<PreparedPath> {
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
    let allocation =
        match Allocation::new(value.clone(), control.peer, inner, provider, 0, keepalive) {
            Ok(allocation) => allocation,
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
        allocation: Some(allocation),
        start,
    };
    let a = prepared.allocation.as_mut().unwrap();
    let mut wire = [0; MAX_FRAME + 1];
    let deadline = Instant::now() + Duration::from_secs(7);
    while !cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
        let now = milliseconds(start);
        for _ in 0..32 {
            match a.socket.recv_from(&mut wire) {
                Ok((n, peer)) if peer == a.peer => match a.rx.open(&wire[..n]) {
                    Some(Packet::Control(Control::PathChallenge {
                        attempt,
                        token,
                        challenge,
                    })) if attempt == a.attempt && token == a.token => {
                        a.phase = Phase::Response(challenge);
                        a.retry = 0;
                    }
                    Some(Packet::Control(Control::PathReady { attempt, token }))
                        if attempt == a.attempt && token == a.token =>
                    {
                        a.phase = Phase::Ready;
                        return Ok(prepared);
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(e) if transient(&e) => break,
                Err(e) => return Err(e),
            }
        }
        if now >= a.retry {
            let c = match a.phase {
                Phase::Response(challenge) => Control::PathResponse {
                    attempt: a.attempt,
                    token: a.token,
                    challenge,
                },
                _ => Control::PathOpen {
                    attempt: a.attempt,
                    token: a.token,
                },
            };
            a.send(c, now, false, keepalive);
            a.retry = now + 500;
        }
        thread::sleep(Duration::from_millis(2));
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
pub fn run_prepared(
    settings: ClientConfig,
    prepared: PreparedPath,
    packet: Box<dyn PacketIo>,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    running: Arc<AtomicBool>,
) -> io::Result<()> {
    if settings.entry != prepared.entry || settings.inner != prepared.inner {
        return Err(io::Error::other(
            "prepared path does not match client settings",
        ));
    }
    run_inner(
        settings,
        None,
        packet,
        provider,
        events,
        running,
        Some(prepared),
    )
}

pub fn run(
    settings: ClientConfig,
    control: Client,
    packet: Box<dyn PacketIo>,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    running: Arc<AtomicBool>,
) -> io::Result<()> {
    run_inner(
        settings,
        Some(control),
        packet,
        provider,
        events,
        running,
        None,
    )
}

fn run_inner(
    settings: ClientConfig,
    control: Option<Client>,
    mut packet: Box<dyn PacketIo>,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    running: Arc<AtomicBool>,
    prepared: Option<PreparedPath>,
) -> io::Result<()> {
    let entry = settings.entry;
    let inner = settings.inner;
    if !(5..=45).contains(&settings.keepalive_seconds) {
        return Err(io::Error::other("invalid keepalive interval"));
    }
    let keepalive = settings.keepalive_seconds;
    let flags = unsafe { libc::fcntl(packet.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(packet.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    provider.packet_ready()?;
    let (start, mut allocations, control) = match prepared {
        Some(mut path) => (
            path.start,
            vec![path.allocation.take().unwrap()],
            path.control.take().unwrap(),
        ),
        None => (Instant::now(), Vec::new(), control.unwrap()),
    };
    let mut http = Some(control);
    let mut job: Option<Job> = None;
    let mut releases: VecDeque<Uuid> = VecDeque::new();
    let mut active = None;
    let mut activation = 0u64;
    let mut tun = [0; 1201];
    let mut wire = [0; MAX_FRAME + 1];
    let mut writes: VecDeque<(Uuid, u64, Vec<u8>)> = VecDeque::new();
    let mut next_prepare = 0;
    let mut recreate = false;
    let (mut ipv6_drops, mut stats_at) = (0u64, 10000u64);
    let result = (|| {
        while running.load(Ordering::Relaxed) {
            let now = milliseconds(start);
            if now >= stats_at {
                events(
                    json!({"event":"stats","protocol":"rtp-http-v2","inner_ipv6_drops":ipv6_drops}),
                );
                stats_at = now.saturating_add(10000);
            }
            if job.as_ref().is_some_and(|t| t.is_finished()) {
                let (client, result) = job
                    .take()
                    .unwrap()
                    .join()
                    .map_err(|_| io::Error::other("HTTP preparation panicked"))?;
                http = Some(client);
                if result
                    .as_ref()
                    .is_err_and(|e| e.kind() == io::ErrorKind::AddrNotAvailable)
                {
                    // The platform must replace TUN using a fresh preparation; keep its retained
                    // interface installed while this worker closes only its own descriptor.
                    return Err(result.err().unwrap());
                }
                if let Ok(JobResult::Create(value)) = result {
                    match Allocation::new(
                        value.clone(),
                        entry,
                        inner,
                        provider.as_ref(),
                        now,
                        keepalive,
                    ) {
                        Ok(mut a) => {
                            a.activate_early = recreate;
                            if allocations
                                .first()
                                .is_some_and(|old| old.generation != a.generation)
                            {
                                allocations.clear();
                                active = None;
                                writes.clear();
                            }
                            allocations.push(a);
                            recreate = false;
                        }
                        Err(_) => {
                            if let Ok(id) = serde_json::from_value(value["allocation_id"].clone()) {
                                releases.push_back(id);
                            }
                            next_prepare = now + 1000;
                        }
                    }
                }
            }
            if job.is_none() {
                if let Some(id) = releases.pop_front() {
                    job = Some(release_job(http.take().unwrap(), id));
                } else if now >= next_prepare
                    && (allocations.is_empty()
                        || recreate
                        || (allocations.len() == 1
                            && allocations[0]
                                .activated
                                .is_some_and(|at| now >= at + 290_000)))
                {
                    if allocations.len() < 2 {
                        job = Some(create_job(http.take().unwrap(), inner, running.clone()));
                        next_prepare = now + 1000;
                    }
                }
            }
            let rotation_due = active
                .and_then(|id| {
                    allocations
                        .iter()
                        .find(|a| a.id == id)
                        .and_then(|a| a.activated)
                })
                .map_or(0, |at| at + 300_000);
            let mut acknowledged = None;
            for a in &mut allocations {
                if a.retire.is_some_and(|at| now >= at) {
                    continue;
                }
                if a.old_socket.as_ref().is_some_and(|(at, _)| now >= *at) {
                    a.old_socket = None;
                }
                // Receive both old and candidate sockets, sharing the same replay
                // window/counter owner across NAT repair.
                let mut inbound = Vec::new();
                for socket in std::iter::once(&a.socket)
                    .chain(a.candidate.iter())
                    .chain(a.old_socket.iter().map(|(_, s)| s))
                {
                    for _ in 0..32 {
                        match socket.recv_from(&mut wire) {
                            Ok((n, peer)) if peer == a.peer => {
                                if let Some(packet) = a.rx.open(&wire[..n]) {
                                    inbound.push(packet);
                                }
                            }
                            Ok(_) => {}
                            Err(e) if transient(&e) => break,
                            Err(_) => break,
                        }
                    }
                }
                for received in inbound {
                    match received {
                        Packet::Data(ip) => {
                            a.health.received(now, None);
                            if writes.len() < 218 {
                                writes.push_back((a.id, now, ip));
                            }
                        }
                        Packet::Control(c) => {
                            a.health.received(
                                now,
                                if let Control::Pong(token) = c {
                                    Some(token)
                                } else {
                                    None
                                },
                            );
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
                                    && matches!(a.phase,Phase::Activating(n) if n==ack) =>
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
                        || active.is_none()
                        || now >= rotation_due)
                {
                    activation = activation
                        .max(a.activation_floor - 1)
                        .checked_add(1)
                        .ok_or_else(invalid)?;
                    a.phase = Phase::Activating(activation);
                    a.started = now;
                    a.retry = 0;
                }
                if now >= a.retry {
                    let control = match a.phase {
                        Phase::Open => Some(Control::PathOpen {
                            attempt: a.attempt,
                            token: a.token,
                        }),
                        Phase::Response(challenge) => Some(Control::PathResponse {
                            attempt: a.attempt,
                            token: a.token,
                            challenge,
                        }),
                        Phase::Ready => Some(Control::PathOpen {
                            attempt: a.attempt,
                            token: a.token,
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
                    if provider.repair_requested() {
                        let _ = a.repair(provider.as_ref(), now);
                    }
                    match a.health.poll(now, random()) {
                        Some(HealthAction::Ping(token)) => {
                            a.send(Control::Ping(token), now, false, keepalive)
                        }
                        Some(HealthAction::Suspect) => {
                            events(json!({"event":"repairing"}));
                            if a.repair(provider.as_ref(), now).is_err() || a.candidate.is_none() {
                                recreate = true;
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
                        recreate = true;
                        events(json!({"event":"degraded"}));
                    } else {
                        a.retire = Some(now);
                        next_prepare = now + 1000;
                    }
                }
            }
            if let Some(id) = acknowledged {
                for a in &mut allocations {
                    if a.id != id && a.retire.is_none() {
                        a.retire = Some(now + 10_000);
                    }
                }
                active = Some(id);
                events(json!({"event":"connected","protocol":"rtp-http-v2"}));
            }
            allocations.retain(|a| {
                if a.retire.is_some_and(|at| now >= at) {
                    releases.push_back(a.id);
                    false
                } else {
                    true
                }
            });
            for _ in 0..32 {
                match packet.read(&mut tun) {
                    Ok(n) if n > 0 && tun[0] >> 4 == 6 => {
                        ipv6_drops += 1;
                    }
                    Ok(n) if n <= 1200 => {
                        if let Some(a) =
                            active.and_then(|id| allocations.iter_mut().find(|a| a.id == id))
                        {
                            if let Some(frame) = a.tx.data(&tun[..n], (now as u32).wrapping_mul(90))
                            {
                                if a.socket.send_to(&frame, a.peer).is_ok() {
                                    a.health.sent(now, idle_delay(keepalive));
                                }
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) if transient(&e) => break,
                    Err(e) => return Err(e),
                }
            }
            for _ in 0..32 {
                let Some((id, at, ip)) = writes.front() else {
                    break;
                };
                if now.saturating_sub(*at) >= 1000 || !allocations.iter().any(|a| a.id == *id) {
                    writes.pop_front();
                    continue;
                }
                match packet.write(ip) {
                    Ok(n) if n == ip.len() => {
                        writes.pop_front();
                    }
                    Ok(_) => return Err(io::Error::other("partial packet write")),
                    Err(e) if transient(&e) => break,
                    Err(e) => return Err(e),
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    })();
    running.store(false, Ordering::Relaxed);
    if let Some(job) = job {
        let (client, created) = job
            .join()
            .map_err(|_| io::Error::other("HTTP owner panicked"))?;
        http = Some(client);
        if let Ok(JobResult::Create(value)) = created {
            if let Ok(id) = serde_json::from_value(value["allocation_id"].clone()) {
                releases.push_back(id);
            }
        }
    }
    if let Some(mut client) = http {
        for id in allocations.iter().map(|a| a.id).chain(releases) {
            let _ = client.call(
                "release_udp",
                json!({"allocation_id":id}).as_object().unwrap().clone(),
            );
        }
    }
    result
}
