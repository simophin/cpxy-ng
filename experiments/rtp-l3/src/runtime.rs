// Included inside the Linux module so the portable library stays socket-free.
use control::{open_packet, Control, Packet, PortKeys, MAX_CONTROL_IPV4};
use rand::{rngs::OsRng, Rng, RngCore};
use state::{keepalive_delay, rotation_delay, Client, Path, Send, Server};
use std::collections::BTreeMap;

struct Stream {
    keys: PortKeys,
    tx: Tx,
}
impl Stream {
    fn new(keys: PortKeys) -> Self {
        Self {
            tx: Tx::new(keys.tx_data),
            keys,
        }
    }
}
fn nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn prepare(socket: &UdpSocket) -> io::Result<()> {
    if !socket.local_addr()?.is_ipv4() {
        return Err(io::Error::other("IPv4 socket required"));
    }
    let enabled: libc::c_int = 1;
    if unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RXQ_OVFL,
            &enabled as *const _ as *const _,
            4,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let size: libc::c_int = 4 * 1024 * 1024;
    for opt in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &size as *const _ as *const _,
                4,
            );
        }
    }
    nonblocking(socket.as_raw_fd())
}
fn disconnect(socket: &UdpSocket) -> io::Result<SocketAddr> {
    let peer = socket.peer_addr()?;
    if !peer.is_ipv4() {
        return Err(io::Error::other("IPv4 server required"));
    }
    let local = socket.local_addr()?;
    let addr = libc::sockaddr {
        sa_family: libc::AF_UNSPEC as _,
        sa_data: [0; 14],
    };
    if unsafe { libc::connect(socket.as_raw_fd(), &addr, std::mem::size_of_val(&addr) as _) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Linux may clear an implicitly bound port on AF_UNSPEC. Restore the
    // original tuple, in the socket's original netns, before sending again.
    if socket.local_addr()?.port() != local.port() {
        let SocketAddr::V4(local) = local else {
            unreachable!()
        };
        let addr = libc::sockaddr_in {
            sin_family: libc::AF_INET as _,
            sin_port: local.port().to_be(),
            sin_addr: libc::in_addr {
                s_addr: u32::from_ne_bytes(local.ip().octets()),
            },
            sin_zero: [0; 8],
        };
        if unsafe {
            libc::bind(
                socket.as_raw_fd(),
                &addr as *const _ as *const _,
                std::mem::size_of_val(&addr) as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    if socket.local_addr()?.port() != local.port() {
        return Err(io::Error::other("disconnect changed local port"));
    }
    Ok(peer)
}
fn cidr(s: &str) -> io::Result<(Ipv4Addr, u8)> {
    let (ip, prefix) = s
        .split_once('/')
        .ok_or_else(|| io::Error::other("invalid CIDR"))?;
    let ip = ip
        .parse()
        .map_err(|_| io::Error::other("invalid IPv4 CIDR"))?;
    let prefix = prefix
        .parse::<u8>()
        .ok()
        .filter(|p| *p <= 32)
        .ok_or_else(|| io::Error::other("invalid prefix"))?;
    Ok((ip, prefix))
}
fn bind_rotation(
    address: SocketAddr,
    low: u16,
    high: u16,
    occupied: &[u16],
) -> io::Result<(u16, UdpSocket)> {
    let count = u32::from(high) - u32::from(low) + 1;
    let offset = OsRng.gen_range(0..count);
    for i in 0..count {
        let port = (u32::from(low) + (offset + i) % count) as u16;
        if occupied.contains(&port) {
            continue;
        }
        let mut addr = address;
        addr.set_port(port);
        match UdpSocket::bind(addr) {
            Ok(s) => {
                prepare(&s)?;
                return Ok((port, s));
            }
            Err(e)
                if e.kind() == io::ErrorKind::AddrInUse
                    || e.kind() == io::ErrorKind::PermissionDenied =>
            {
                continue
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no available rotation port"))
}
struct Runtime {
    psk: [u8; 32],
    base: u16,
    peer: Option<SocketAddr>,
    client: Option<Client>,
    server: Option<Server>,
    sockets: BTreeMap<u16, UdpSocket>,
    streams: BTreeMap<u16, Stream>,
    drops: BTreeMap<u16, u32>,
    counts: Counts,
    started: Instant,
}
impl Runtime {
    fn ticks(&self) -> u32 {
        (self.started.elapsed().as_nanos() * 90000 / 1000000000) as u32
    }
    fn sync_client(&mut self, now: u64) {
        let c = self.client.as_ref().unwrap();
        self.streams.retain(|port, _| c.knows(*port, now));
        for p in c.rotation.iter().chain(c.previous.iter().map(|(p, _)| p)) {
            if !c.knows(p.port, now) {
                continue;
            }
            let keys = PortKeys::rotation(&self.psk, &p.secret, true);
            if self.streams.get(&p.port).is_none_or(|s| s.keys != keys) {
                self.streams.insert(p.port, Stream::new(keys));
            }
        }
    }
    fn send(&mut self, path: Path, packet: &[u8]) -> bool {
        let socket = self.sockets.get(&if self.client.is_some() {
            self.base
        } else {
            path.port
        });
        if socket.and_then(|s| s.send_to(packet, path.peer).ok()) == Some(packet.len()) {
            true
        } else {
            self.counts.socket_drops += 1;
            false
        }
    }
    fn control(&mut self, action: Send) {
        let ticks = self.ticks();
        if let Some(s) = self.streams.get_mut(&action.path.port) {
            let p =
                s.tx.seal_control(&s.keys.tx_control, &action.message, ticks, MAX_CONTROL_IPV4)
                    .unwrap();
            if matches!(action.message, Control::Ping { .. }) {
                self.counts.pings_sent += 1;
            }
            if self.send(action.path, &p) {
                self.counts.control_tx += 1;
                self.counts.tx_wire += p.len() as u64;
            }
        }
    }
    fn ping(&mut self, msg: Control) {
        let mut peer = self.peer.unwrap();
        peer.set_port(self.client.as_ref().unwrap().current);
        self.control(Send {
            path: Path {
                port: peer.port(),
                peer,
            },
            message: msg,
        });
    }
    fn close(&mut self, port: u16) {
        self.sockets.remove(&port);
        self.streams.remove(&port);
        self.drops.remove(&port);
    }
    fn incoming(&mut self, port: u16, source: SocketAddr, b: &[u8], now: u64, dev: &mut fs::File) {
        if let Some(peer) = self.peer {
            if source.ip() != peer.ip() || !self.client.as_ref().unwrap().knows(source.port(), now)
            {
                self.counts.invalid += 1;
                return;
            }
        }
        let port = if self.client.is_some() {
            source.port()
        } else {
            port
        };
        let Some(s) = self.streams.get(&port) else {
            self.counts.invalid += 1;
            return;
        };
        match open_packet(&s.keys.rx_data, &s.keys.rx_control, b) {
            Some(Packet::Data(p)) => {
                let valid = if let Some(c) = &mut self.client {
                    c.received_data(port, now)
                } else {
                    self.server
                        .as_mut()
                        .unwrap()
                        .upstream(Path { port, peer: source }, now)
                };
                if !valid {
                    self.counts.invalid += 1;
                    return;
                }
                self.counts.rx_packets += 1;
                self.counts.rx_plain += p.len() as u64;
                self.counts.rx_wire += b.len() as u64;
                if dev.write(&p).ok() != Some(p.len()) {
                    self.counts.tun_drops += 1;
                }
            }
            Some(Packet::Control(msg)) => {
                self.counts.control_rx += 1;
                self.counts.rx_wire += b.len() as u64;
                if let Some(c) = &mut self.client {
                    if !matches!(msg, Control::State { .. }) {
                        self.counts.control_rejected += 1;
                        return;
                    }
                    let reply = c.state(port, &msg, now);
                    if reply.is_some() {
                        self.counts.switches += 1;
                    }
                    self.sync_client(now);
                    if let Some(msg) = reply {
                        self.ping(msg);
                    }
                } else if matches!(msg, Control::Ping { .. }) {
                    if let Some((reply, _)) =
                        self.server
                            .as_mut()
                            .unwrap()
                            .ping(Path { port, peer: source }, &msg, now)
                    {
                        self.control(reply);
                    } else {
                        self.counts.control_rejected += 1;
                    }
                } else {
                    self.counts.control_rejected += 1;
                }
            }
            None => {
                // Encrypted malformed traffic cannot reliably be classified as
                // data vs control; count decoder failures in both categories.
                self.counts.invalid += 1;
                self.counts.control_rejected += 1;
            }
        }
    }
}
pub fn main() -> io::Result<()> {
    let a: Vec<String> = env::args().collect();
    let client = a.get(1).map(String::as_str) == Some("client");
    if !(client && a.len() == 6
        || a.get(1).map(String::as_str) == Some("server") && (7..=8).contains(&a.len()))
    {
        return Err(io::Error::other("usage: rtp-l3-poc client KEY IFACE CIDR FD | server KEY IFACE CIDR LISTEN_BASE LOW-HIGH [ROTATE_SECS]"));
    }
    let psk = key(&a[2])?;
    let (inner, prefix) = cidr(&a[4])?;
    let mean = a
        .get(7)
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|_| io::Error::other("invalid rotation interval"))?
        .unwrap_or(300);
    let mut next_rotation =
        rotation_delay(mean).ok_or_else(|| io::Error::other("rotation interval out of range"))?;
    let keepalive = env::var("POC_KEEPALIVE_SECS")
        .map(|v| v.parse::<u64>())
        .unwrap_or(Ok(15))
        .map_err(|_| io::Error::other("invalid keepalive interval"))?;
    keepalive_delay(keepalive)
        .ok_or_else(|| io::Error::other("keepalive interval out of range"))?;
    // The fixed 60-second grace must cover maximum jitter plus all retries.
    if keepalive > 45 {
        return Err(io::Error::other(
            "keepalive mean must be at most 45 seconds for 60-second grace",
        ));
    }
    let socket = if client {
        let fd = a[5]
            .parse::<i32>()
            .ok()
            .filter(|f| *f >= 3)
            .ok_or_else(|| io::Error::other("invalid inherited FD"))?;
        unsafe { UdpSocket::from_raw_fd(fd) }
    } else {
        UdpSocket::bind(&a[5])?
    };
    prepare(&socket)?;
    let peer = if client {
        Some(disconnect(&socket)?)
    } else {
        None
    };
    let address = socket.local_addr()?;
    let base = peer.unwrap_or(address).port();
    let mut rt = Runtime {
        psk,
        base,
        peer,
        client: None,
        server: None,
        sockets: BTreeMap::from([(base, socket)]),
        streams: BTreeMap::from([(base, Stream::new(PortKeys::base(&psk, client)))]),
        drops: BTreeMap::new(),
        counts: Counts::default(),
        started: Instant::now(),
    };
    let (low, high) = if client {
        (0, 0)
    } else {
        let (l, h) = a[6]
            .split_once('-')
            .ok_or_else(|| io::Error::other("invalid rotation range"))?;
        let l = l
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| io::Error::other("invalid low port"))?;
        let h = h
            .parse::<u16>()
            .ok()
            .filter(|p| *p >= l)
            .ok_or_else(|| io::Error::other("invalid high port"))?;
        // Leave a candidate available beside the current and previous ports.
        if u32::from(h) - u32::from(l) + 1 - u32::from((l..=h).contains(&base)) < 3 {
            return Err(io::Error::other(
                "rotation range needs at least three non-base ports",
            ));
        }
        (l, h)
    };
    if client {
        rt.client = Client::new(
            OsRng.next_u64(),
            inner,
            base,
            keepalive_delay(keepalive).unwrap(),
            0,
        );
    } else {
        let (port, socket) = bind_rotation(address, low, high, &[base])?;
        let mut secret = [0; 32];
        OsRng.fill_bytes(&mut secret);
        rt.server = Server::new(
            OsRng.next_u64(),
            inner,
            prefix,
            base,
            port,
            secret,
            60,
            90,
            1024,
        );
        rt.sockets.insert(port, socket);
        rt.streams
            .insert(port, Stream::new(PortKeys::rotation(&psk, &secret, false)));
    }
    let mut dev = tun(&a[3], &a[4])?;
    nonblocking(dev.as_raw_fd())?;
    if client {
        ip(&["link", "set", "lo", "up"])?;
    }
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    if let Some(c) = &mut rt.client {
        let ping = c.ping(0).unwrap();
        rt.ping(ping);
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"ready","client":client,"protocol":"xchacha20-v4","mtu":MTU})
    );
    let mut last = Instant::now();
    let mut b = [0; 65536];
    while RUN.load(Ordering::Relaxed) {
        let now = rt.started.elapsed().as_secs();
        if let Some(c) = &mut rt.client {
            let ping = c.tick(now);
            rt.counts.fallbacks = c.fallbacks;
            rt.counts.pings_unanswered = c.pings_unanswered;
            rt.counts.sessions = usize::from(c.instance.is_some());
            if ping.is_some() {
                c.set_keepalive_interval(keepalive_delay(keepalive).unwrap());
            }
            rt.sync_client(now);
            if let Some(ping) = ping {
                rt.ping(ping);
            }
        } else {
            if now >= next_rotation {
                match bind_rotation(address, low, high, &rt.server.as_ref().unwrap().ports()) {
                    Ok((port, socket)) => {
                        let mut secret = [0; 32];
                        OsRng.fill_bytes(&mut secret);
                        if let Some((actions, closed)) =
                            rt.server.as_mut().unwrap().rotate(port, secret, now)
                        {
                            if let Some(port) = closed {
                                rt.close(port);
                            }
                            rt.sockets.insert(port, socket);
                            rt.streams.insert(
                                port,
                                Stream::new(PortKeys::rotation(&psk, &secret, false)),
                            );
                            rt.counts.rotations += 1;
                            rt.counts.announces_sent += actions.len() as u64;
                            for action in actions {
                                rt.control(action);
                            }
                        }
                    }
                    Err(_) => rt.counts.rotation_bind_failures += 1,
                }
                next_rotation = now.saturating_add(rotation_delay(mean).unwrap());
            }
            let (actions, closed) = rt.server.as_mut().unwrap().tick(now);
            rt.counts.announce_retries += actions.len() as u64;
            for action in actions {
                rt.control(action);
            }
            for port in closed {
                rt.close(port);
            }
            rt.counts.sessions = rt.server.as_ref().unwrap().sessions().len();
            rt.counts.session_conflicts = rt.server.as_ref().unwrap().conflicts;
        }
        let ports: Vec<_> = rt.sockets.keys().copied().collect();
        let mut polls: Vec<_> = rt
            .sockets
            .values()
            .map(|s| libc::pollfd {
                fd: s.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        polls.push(libc::pollfd {
            fd: dev.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, 250) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        let now = rt.started.elapsed().as_secs();
        for (index, port) in ports.iter().enumerate() {
            if polls[index].revents & libc::POLLIN == 0 {
                continue;
            }
            for _ in 0..64 {
                match receive(&rt.sockets[port], &mut b) {
                    Ok((n, source, drops)) => {
                        if let Some(d) = drops {
                            let prev = rt.drops.insert(*port, d).unwrap_or(0);
                            rt.counts.kernel_rx_drops += d.wrapping_sub(prev) as u64;
                        }
                        rt.incoming(*port, source, &b[..n], now, &mut dev);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    // ICMP errors from a closed rotation port must not kill fallback.
                    Err(e)
                        if matches!(
                            e.raw_os_error(),
                            Some(libc::ECONNREFUSED | libc::EHOSTUNREACH | libc::ENETUNREACH)
                        ) =>
                    {
                        break
                    }
                    Err(e) if e.kind() == io::ErrorKind::Other => {
                        rt.counts.invalid += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if polls.last().unwrap().revents & libc::POLLIN != 0 {
            for _ in 0..64 {
                match dev.read(&mut b) {
                    Ok(n) => {
                        if n < 20 {
                            rt.counts.tun_drops += 1;
                            continue;
                        }
                        let path = if let Some(c) = &rt.client {
                            let mut peer = rt.peer.unwrap();
                            peer.set_port(c.current);
                            Some(Path {
                                port: c.current,
                                peer,
                            })
                        } else {
                            rt.server
                                .as_mut()
                                .unwrap()
                                .downstream(Ipv4Addr::new(b[16], b[17], b[18], b[19]), now)
                        };
                        let Some(path) = path else {
                            rt.counts.unknown_destination += 1;
                            continue;
                        };
                        let ticks = rt.ticks();
                        let packet = rt
                            .streams
                            .get_mut(&path.port)
                            .and_then(|s| s.tx.seal(&b[..n], ticks));
                        if let Some(p) = packet {
                            if rt.send(path, &p) {
                                rt.counts.tx_packets += 1;
                                rt.counts.tx_plain += n as u64;
                                rt.counts.tx_wire += p.len() as u64;
                                if let Some(c) = &mut rt.client {
                                    c.sent_data(now);
                                }
                            }
                        } else {
                            rt.counts.tun_drops += 1;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
        }
        if last.elapsed() >= Duration::from_secs(5) {
            rt.counts.report(rt.started.elapsed().as_secs_f64(), false);
            last = Instant::now();
        }
    }
    rt.counts.report(rt.started.elapsed().as_secs_f64(), true);
    Ok(())
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn disconnect_preserves_tuple_and_sends_to_multiple_ports() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let fixed_port = reservation.local_addr().unwrap().port();
        drop(reservation);
        for bind_port in [None, Some(0u16), Some(fixed_port)] {
            let a = UdpSocket::bind("127.0.0.1:0").unwrap();
            let b = UdpSocket::bind("127.0.0.1:0").unwrap();
            let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
            assert!(fd >= 0);
            let socket = unsafe { UdpSocket::from_raw_fd(fd) };
            if let Some(port) = bind_port {
                let addr = libc::sockaddr_in {
                    sin_family: libc::AF_INET as _,
                    sin_port: port.to_be(),
                    sin_addr: libc::in_addr { s_addr: 0 },
                    sin_zero: [0; 8],
                };
                assert_eq!(
                    unsafe {
                        libc::bind(
                            fd,
                            &addr as *const _ as *const _,
                            std::mem::size_of_val(&addr) as _,
                        )
                    },
                    0
                );
            }
            socket.connect(a.local_addr().unwrap()).unwrap();
            let local = socket.local_addr().unwrap();
            assert_eq!(disconnect(&socket).unwrap(), a.local_addr().unwrap());
            assert_eq!(socket.local_addr().unwrap().port(), local.port());
            for receiver in [&a, &b] {
                receiver
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .send_to(b"probe", receiver.local_addr().unwrap())
                    .unwrap();
                let mut buf = [0; 16];
                let (n, source) = receiver.recv_from(&mut buf).unwrap();
                assert_eq!(&buf[..n], b"probe");
                assert_eq!(source, local);
            }
        }
    }
}
