//! Single TUN dispatcher and independent serialized HTTP/firewall owner.
use super::{
    leases::{Controller, State, Table},
    wire,
};
use crate::{
    addresses::Addresses,
    config::{Role, Settings},
};
use rtp_l3::{
    v2::{
        codec::{Control, Packet, MAX_FRAME},
        http::{random, Server},
    },
    worker::{Events, PacketIo, SocketProvider},
};
use std::{
    collections::VecDeque,
    io,
    net::{Ipv4Addr, TcpListener},
    os::fd::AsRawFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
fn nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
fn ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
pub fn run(
    settings: Settings,
    psk: [u8; 32],
    mut packet: Box<dyn PacketIo>,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    running: Arc<AtomicBool>,
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
    let mut envelope = Server::new(&psk, &settings.control.server_identity, 4096)
        .ok_or_else(|| io::Error::other("invalid control identity"))?;
    let mut controller = Controller::new(
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
    let listener = TcpListener::bind(listen)?;
    let start = Instant::now();
    let (commands, rx) = mpsc::sync_channel::<wire::Request>(64);
    nonblocking(packet.as_raw_fd())?;
    provider.packet_ready()?;
    let listen_run = running.clone();
    let listener_thread = thread::spawn(move || wire::listen(listener, commands, listen_run));
    let control_run = running.clone();
    let control_provider = provider.clone();
    let control_events = events.clone();
    let control_thread = thread::spawn(move || {
        let mut reconcile_at = 0;
        let mut degraded = false;
        while control_run.load(Ordering::Relaxed) {
            let now = ms(start);
            controller.tick(now);
            if now >= reconcile_at {
                let ready = control_provider.reconcile_ports().is_ok();
                if !ready && !degraded {
                    control_events(serde_json::json!({"event":"degraded","component":"admission"}));
                }
                if ready && degraded {
                    control_events(serde_json::json!({"event":"admission_restored"}));
                }
                degraded = !ready;
                reconcile_at = now + 1000;
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(request) if Instant::now() < request.deadline => {
                    let reply =
                        envelope.handle(&request.body, now, |rpc| controller.rpc(rpc, ms(start)));
                    let _ = request.response.try_send(reply);
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        controller.state.shutdown()
    });
    events(serde_json::json!({"event":"ready","protocol":"rtp-http-v2","mtu":1200}));
    let result = packets(
        &mut *packet,
        &table,
        &running,
        start,
        &[&listener_thread, &control_thread],
        &events,
    );
    running.store(false, Ordering::Relaxed);
    let control_result = control_thread
        .join()
        .map_err(|_| io::Error::other("control owner panicked"))
        .and_then(|r| r);
    let listener_result = listener_thread
        .join()
        .map_err(|_| io::Error::other("control listener panicked"))
        .and_then(|r| r);
    result.and(control_result).and(listener_result)
}
fn packets(
    packet: &mut dyn PacketIo,
    table: &Arc<Mutex<Table>>,
    running: &AtomicBool,
    start: Instant,
    workers: &[&thread::JoinHandle<io::Result<()>>],
    events: &Events,
) -> io::Result<()> {
    let mut tun = [0; 1201];
    let mut wire = [0; MAX_FRAME + 1];
    let mut writes: VecDeque<(uuid::Uuid, u64, Vec<u8>)> = VecDeque::new();
    let (mut rejected, mut ipv6_drops, mut stats_at) = (0u64, 0u64, 10000u64);
    while running.load(Ordering::Relaxed) {
        if workers.iter().any(|w| w.is_finished()) {
            return Err(io::Error::other("HTTP control worker stopped"));
        }
        let now = ms(start);
        let ticks = (now as u32).wrapping_mul(90);
        let mut guard = table.lock().unwrap();
        let Table {
            allocations,
            routes,
            queues,
        } = &mut *guard;
        for (id, a) in allocations.iter_mut() {
            if a.retired {
                continue;
            }
            for _ in 0..32 {
                let (n, peer) = match a.socket.recv_from(&mut wire) {
                    Ok(v) => v,
                    Err(e) if transient(&e) => break,
                    Err(_) => {
                        a.retired = true;
                        break;
                    }
                };
                let Some(body) = a.rx.open(&wire[..n]) else {
                    rejected += 1;
                    continue;
                };
                // Decoder authentication/replay checks precede every state effect.
                match body {
                    Packet::Data(ip) => {
                        if !a.path.data_from(peer) || !routes.upstream(a.client, *id, now) {
                            continue;
                        }
                        a.last_seen = Some(now);
                        if writes.len() < 218 {
                            writes.push_back((*id, now, ip));
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
        for _ in 0..32 {
            match packet.read(&mut tun) {
                Ok(n) if n >= 20 && n <= 1200 && tun[0] >> 4 == 4 => {
                    let ip = Ipv4Addr::new(tun[16], tun[17], tun[18], tun[19]);
                    queues.push(ip, tun[..n].to_vec(), now);
                }
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "packet device closed",
                    ))
                }
                Ok(n) if n > 0 && tun[0] >> 4 == 6 => {
                    ipv6_drops += 1;
                }
                Ok(_) => {
                    rejected += 1;
                }
                Err(e) if transient(&e) => break,
                Err(e) => return Err(e),
            }
        }
        // One route/queue owner, no duplicate downstream sends. FIFO is consumed
        // only after successful send; backpressure yields to the next turn.
        for (id, a) in allocations.iter_mut() {
            if a.retired || routes.owner(a.client) != Some(*id) {
                continue;
            }
            let Some(path) = a.path.committed else {
                continue;
            };
            for _ in 0..32 {
                let Some(body) = queues.front(a.inner, now) else {
                    break;
                };
                let Some(frame) = a.tx.data(body, ticks) else {
                    queues.pop(a.inner);
                    continue;
                };
                match a.socket.send_to(&frame, path.peer) {
                    Ok(_) => queues.pop(a.inner),
                    Err(e) if transient(&e) => break,
                    Err(_) => {
                        a.retired = true;
                        break;
                    }
                }
            }
        }
        if now >= stats_at {
            events(
                serde_json::json!({"event":"stats","protocol":"rtp-http-v2","inner_ipv6_drops":ipv6_drops,"rejected":rejected,"queue_expired":queues.expired,"queue_pressure":queues.pressure,"unknown_destination":queues.unknown}),
            );
            stats_at = now.saturating_add(10000);
        }
        let sockets: Vec<_> = allocations
            .values()
            .filter(|a| !a.retired)
            .map(|a| a.socket.as_raw_fd())
            .collect();
        drop(guard);
        for _ in 0..32 {
            let Some((id, at, ip)) = writes.front() else {
                break;
            };
            let ownership = table.lock().unwrap();
            if now.saturating_sub(*at) >= 1000 || !ownership.allocations.contains_key(id) {
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
        rtp_l3::worker::wait_readable(packet, sockets.into_iter())?;
    }
    Ok(())
}
