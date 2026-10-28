//! Single packet dispatcher task and independent serialized HTTP/firewall owner.
use super::{
    leases::{Controller, State, Table},
    wire,
};
use crate::{
    addresses::Addresses,
    config::{Role, Settings},
    network::WritePolicy,
};
use rtp_l3::{
    device::{self, Device},
    v2::{
        codec::{Control, Packet, MAX_FRAME},
        http::{random, Server},
    },
    worker::{Events, SocketProvider},
};
use std::{
    collections::BTreeMap,
    future::poll_fn,
    io,
    net::{Ipv4Addr, UdpSocket},
    os::fd::OwnedFd,
    sync::{mpsc, Arc, Mutex},
    task::Poll,
    time::{Duration, Instant},
};
use tokio::{
    io::{unix::AsyncFd, Interest},
    sync::{mpsc::error::TryRecvError, mpsc::error::TrySendError, Notify},
    task, time,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Wake at least this often for lease expiry, queue ageing and admission reconciliation.
const TICK: Duration = Duration::from_millis(250);
/// Device packets, and datagrams per socket in each direction, handled in one pass.
const BUDGET: usize = 32;

fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
fn ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// Serve the endpoint until `stop` is cancelled or a component fails: the packet dispatcher
/// and TUN pump on the runtime, HTTP connections on blocking tasks, and leases/firewall on one
/// serialized blocking owner.
pub fn run(
    settings: Settings,
    psk: [u8; 32],
    packet: OwnedFd,
    accept: WritePolicy,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    stop: CancellationToken,
) -> io::Result<()> {
    let Role::Endpoint {
        listen,
        udp_low: low,
        udp_high: high,
        ..
    } = settings.transport
    else {
        return Err(io::Error::other("endpoint settings required"));
    };
    let (server, prefix) = settings.inner()?;
    if prefix > 30 {
        return Err(io::Error::other("subnet has no client address pool"));
    }
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    let network = u32::from(server) & mask;
    let mut default_low = network + 1;
    if default_low == u32::from(server) {
        default_low += 1;
    }
    let mut default_high = network | !mask;
    default_high -= 1;
    if default_high == u32::from(server) {
        default_high -= 1;
    }
    let addresses = Addresses::open(
        settings.journal.with_extension("addresses.json"),
        server,
        prefix,
        settings
            .control
            .pool_low
            .unwrap_or_else(|| Ipv4Addr::from(default_low)),
        settings
            .control
            .pool_high
            .unwrap_or_else(|| Ipv4Addr::from(default_high)),
        settings.control.reserved.clone(),
    )?;
    let table = Arc::new(Mutex::new(Table::default()));
    let envelope = Server::new(&psk, &settings.control.server_identity, 4096)
        .ok_or_else(|| io::Error::other("invalid control identity"))?;
    let controller = Controller::new(
        State::new(
            addresses,
            table.clone(),
            provider.clone(),
            listen,
            low,
            high,
            prefix,
            server,
        ),
        envelope.generation.clone(),
    );
    let listener = std::net::TcpListener::bind(listen)?;
    listener.set_nonblocking(true)?;
    super::runtime()?.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let start = Instant::now();
        // Any component ending stops the others; only the caller's token is a clean stop.
        let parts = stop.child_token();
        let (commands, requests) = mpsc::sync_channel::<wire::Request>(64);
        provider.packet_ready()?;
        let changed = Arc::new(Notify::new());
        let control = task::spawn_blocking({
            let (provider, events, changed, parts) = (
                provider.clone(),
                events.clone(),
                changed.clone(),
                parts.clone(),
            );
            move || {
                control(
                    controller, envelope, requests, provider, events, start, changed, parts,
                )
            }
        });
        let http = tokio::spawn({
            let parts = parts.clone();
            async move {
                let result = wire::listen(listener, commands, parts.clone()).await;
                parts.cancel();
                result
            }
        });
        events(serde_json::json!({"event":"ready","protocol":"rtp-http-v2","mtu":1200}));
        let (device, port) = device::channel();
        let pump = tokio::spawn(device::pump(packet, port, accept));
        let result = Dispatcher::new(table, start, events, device, changed)
            .run(&parts)
            .await;
        parts.cancel();
        let control = control
            .await
            .map_err(|_| io::Error::other("control owner panicked"))
            .and_then(|r| r);
        let http = http
            .await
            .map_err(|_| io::Error::other("control listener panicked"))
            .and_then(|r| r);
        let pumped = pump
            .await
            .map_err(|_| io::Error::other("packet pump panicked"))
            .and_then(|r| r);
        let unexpected = if stop.is_cancelled() {
            Ok(())
        } else {
            Err(io::Error::other("HTTP control worker stopped"))
        };
        result.and(pumped).and(control).and(http).and(unexpected)
    })
}

/// The serialized lease and firewall owner. Blocking by design: admission and journal calls
/// must not run on the packet runtime or hold the packet table lock.
#[allow(clippy::too_many_arguments)]
fn control(
    mut controller: Controller,
    mut envelope: Server,
    requests: mpsc::Receiver<wire::Request>,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    start: Instant,
    changed: Arc<Notify>,
    stop: CancellationToken,
) -> io::Result<()> {
    let mut reconcile_at = 0;
    let mut degraded = false;
    while !stop.is_cancelled() {
        let now = ms(start);
        controller.tick(now);
        if now >= reconcile_at {
            let ready = provider.reconcile_ports().is_ok();
            if !ready && !degraded {
                events(serde_json::json!({"event":"degraded","component":"admission"}));
            }
            if ready && degraded {
                events(serde_json::json!({"event":"admission_restored"}));
            }
            degraded = !ready;
            reconcile_at = now + 1000;
        }
        match requests.recv_timeout(TICK) {
            Ok(request) if Instant::now() < request.deadline => {
                let reply =
                    envelope.handle(&request.body, now, |rpc| controller.rpc(rpc, ms(start)));
                let _ = request.response.try_send(reply);
                // New allocations need their sockets watched now, not at the next tick.
                changed.notify_one();
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop.cancel();
    controller.state.shutdown()
}

enum Wake {
    Stop,
    Packet(Option<Vec<u8>>),
    Ready,
}

/// Resolves once any watched socket is readable, or a send-blocked one is writable.
async fn ready(sockets: &BTreeMap<Uuid, AsyncFd<UdpSocket>>, blocked: &[Uuid]) {
    poll_fn(|cx| {
        let readable = sockets.values().any(|s| s.poll_read_ready(cx).is_ready());
        if readable
            || blocked
                .iter()
                .filter_map(|id| sockets.get(id))
                .any(|s| s.poll_write_ready(cx).is_ready())
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The one owner of packet I/O: TUN packets from the pump, and every allocation's UDP.
struct Dispatcher {
    table: Arc<Mutex<Table>>,
    start: Instant,
    events: Events,
    device: Device,
    /// The control owner changed the table.
    changed: Arc<Notify>,
    /// A registered duplicate of each live allocation socket. The control owner keeps and
    /// closes the original; this copy keeps the descriptor valid while it is watched.
    sockets: BTreeMap<Uuid, AsyncFd<UdpSocket>>,
    /// Owners whose downstream sends would block.
    blocked: Vec<Uuid>,
    /// A downstream queue still had packets when its budget ran out.
    more: bool,
    rejected: u64,
    ipv6_drops: u64,
    device_queue_full: u64,
    stats_at: u64,
    wire: Box<[u8; MAX_FRAME + 1]>,
}

impl Dispatcher {
    fn new(
        table: Arc<Mutex<Table>>,
        start: Instant,
        events: Events,
        device: Device,
        changed: Arc<Notify>,
    ) -> Self {
        Self {
            table,
            start,
            events,
            device,
            changed,
            sockets: BTreeMap::new(),
            blocked: Vec::new(),
            more: false,
            rejected: 0,
            ipv6_drops: 0,
            device_queue_full: 0,
            stats_at: 10_000,
            wire: Box::new([0; MAX_FRAME + 1]),
        }
    }

    async fn run(mut self, stop: &CancellationToken) -> io::Result<()> {
        let mut first = None;
        loop {
            self.service(ms(self.start), first.take())?;
            let wake = if self.more {
                task::yield_now().await;
                if stop.is_cancelled() {
                    Wake::Stop
                } else {
                    Wake::Ready
                }
            } else {
                tokio::select! {
                    () = stop.cancelled() => Wake::Stop,
                    packet = self.device.reads.recv() => Wake::Packet(packet),
                    () = ready(&self.sockets, &self.blocked) => Wake::Ready,
                    () = self.changed.notified() => Wake::Ready,
                    () = time::sleep(TICK) => Wake::Ready,
                }
            };
            match wake {
                Wake::Stop => return Ok(()),
                Wake::Packet(None) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "packet device closed",
                    ))
                }
                Wake::Packet(packet) => first = packet,
                Wake::Ready => {}
            }
        }
    }

    fn service(&mut self, now: u64, first: Option<Vec<u8>>) -> io::Result<()> {
        let ticks = (now as u32).wrapping_mul(90);
        let mut guard = self.table.lock().unwrap();
        let Table {
            allocations,
            routes,
            queues,
        } = &mut *guard;
        self.sockets
            .retain(|id, _| allocations.get(id).is_some_and(|a| !a.retired));
        for (id, a) in allocations.iter() {
            if a.retired || self.sockets.contains_key(id) {
                continue;
            }
            // Without a watch the socket is still served, on the next tick.
            if let Ok(copy) = a.socket.try_clone() {
                // SAFETY: the AsyncFd owns this duplicate, so it stays open while registered.
                if let Ok(watch) = unsafe { AsyncFd::register(copy) } {
                    self.sockets.insert(*id, watch);
                }
            }
        }
        let Self {
            sockets,
            device,
            wire,
            rejected,
            device_queue_full,
            ..
        } = self;
        for (id, a) in allocations.iter_mut() {
            if a.retired {
                continue;
            }
            let watch = sockets.get(id);
            for _ in 0..BUDGET {
                let received = match watch {
                    Some(watch) => watch.try_io(Interest::READABLE, |s| s.recv_from(&mut wire[..])),
                    None => a.socket.recv_from(&mut wire[..]),
                };
                let (n, peer) = match received {
                    Ok(v) => v,
                    Err(e) if transient(&e) => break,
                    Err(_) => {
                        a.retired = true;
                        break;
                    }
                };
                let Some(body) = a.rx.open(&wire[..n]) else {
                    *rejected += 1;
                    continue;
                };
                // Decoder authentication/replay checks precede every state effect.
                match body {
                    Packet::Data(ip) => {
                        if !a.path.data_from(peer) || !routes.upstream(a.client, *id, now) {
                            continue;
                        }
                        a.last_seen = Some(now);
                        match device.writes.try_send(ip) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => *device_queue_full += 1,
                            Err(TrySendError::Closed(_)) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::UnexpectedEof,
                                    "packet device closed",
                                ))
                            }
                        }
                    }
                    Packet::Control(control) => {
                        let reply = match control {
                            Control::PathOpen { attempt, token } => {
                                a.path.open(peer, attempt, token, random(), now)
                            }
                            Control::PathResponse {
                                attempt,
                                token,
                                challenge,
                            } => a.path.response(peer, attempt, token, challenge, now),
                            Control::Activate {
                                activation,
                                attempt,
                                token,
                            } if a.path.can_activate(peer, attempt, token) => {
                                // Equal activation only acknowledges the identical path;
                                // a repaired tuple requires a new ordered activation.
                                let equal_ok = a.activation != Some(activation)
                                    || a.path.committed == a.path.ready;
                                if equal_ok
                                    && routes.activate(a.client, *id, activation, now).is_ok()
                                {
                                    a.activation = Some(activation);
                                    a.path.committed = a.path.ready;
                                    Some(Control::Activated {
                                        activation,
                                        attempt,
                                        token,
                                    })
                                } else {
                                    None
                                }
                            }
                            Control::Ping(probe) if a.path.data_from(peer) => {
                                Some(Control::Pong(probe))
                            }
                            _ => None,
                        };
                        if let Some(control) = reply {
                            a.last_seen = Some(now);
                            if let Some(frame) = a.tx.control(control, ticks) {
                                let _ = a.socket.send_to(&frame, peer);
                            } else {
                                a.retired = true;
                            }
                        }
                    }
                }
            }
        }
        let mut take = first
            .into_iter()
            .map(Ok)
            .chain(std::iter::repeat_with(|| device.reads.try_recv()));
        for _ in 0..BUDGET {
            let tun = match take.next().unwrap() {
                Ok(packet) => packet,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "packet device closed",
                    ))
                }
            };
            let n = tun.len();
            if (20..=1200).contains(&n) && tun[0] >> 4 == 4 {
                let ip = Ipv4Addr::new(tun[16], tun[17], tun[18], tun[19]);
                queues.push(ip, tun, now);
            } else if n > 0 && tun[0] >> 4 == 6 {
                self.ipv6_drops += 1;
            } else {
                *rejected += 1;
            }
        }
        // One route/queue owner, no duplicate downstream sends. FIFO is consumed
        // only after successful send; backpressure waits for the socket to drain.
        self.blocked.clear();
        self.more = false;
        for (id, a) in allocations.iter_mut() {
            if a.retired || routes.owner(a.client) != Some(*id) {
                continue;
            }
            let Some(path) = a.path.committed else {
                continue;
            };
            let watch = sockets.get(id);
            for sent in 0..=BUDGET {
                let Some(body) = queues.front(a.inner, now) else {
                    break;
                };
                if sent == BUDGET {
                    self.more = true;
                    break;
                }
                let Some(frame) = a.tx.data(body, ticks) else {
                    queues.pop(a.inner);
                    continue;
                };
                let result = match watch {
                    Some(watch) => {
                        watch.try_io(Interest::WRITABLE, |s| s.send_to(&frame, path.peer))
                    }
                    None => a.socket.send_to(&frame, path.peer),
                };
                match result {
                    Ok(_) => queues.pop(a.inner),
                    Err(e) if transient(&e) => {
                        self.blocked.push(*id);
                        break;
                    }
                    Err(_) => {
                        a.retired = true;
                        break;
                    }
                }
            }
        }
        if now >= self.stats_at {
            (self.events)(serde_json::json!({
                "event": "stats",
                "protocol": "rtp-http-v2",
                "inner_ipv6_drops": self.ipv6_drops,
                "rejected": self.rejected,
                "queue_expired": queues.expired,
                "queue_pressure": queues.pressure,
                "unknown_destination": queues.unknown,
                "device_queue_full": self.device_queue_full,
            }));
            self.stats_at = now.saturating_add(10000);
        }
        Ok(())
    }
}
