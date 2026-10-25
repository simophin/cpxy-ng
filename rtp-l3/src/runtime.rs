// Included inside the Linux module so the portable library stays socket-free.
use control::{open_packet, Control, Packet, PortKeys, MAX_CONTROL_IPV4};
use rand::{rngs::OsRng, Rng, RngCore};
use state::{keepalive_delay, rotation_delay, Client, Path, Send, Server};
use std::collections::BTreeMap;

fn control_metadata(message: &Control) -> serde_json::Value {
    match message {
        Control::Ping {
            session,
            seq,
            epoch,
            ..
        } => serde_json::json!({"kind":"ping", "session":session, "seq":seq, "epoch":epoch}),
        Control::Probe {
            session,
            seq,
            epoch,
            ..
        } => serde_json::json!({"kind":"probe", "session":session, "seq":seq, "epoch":epoch}),
        Control::State {
            session,
            echo,
            epoch,
            ..
        } => serde_json::json!({"kind":"state", "session":session, "echo":echo, "epoch":epoch}),
    }
}

pub fn broker_socket(broker: &fs::File, port: u16) -> io::Result<UdpSocket> {
    let request = port.to_be_bytes();
    unsafe {
        if libc::send(
            broker.as_raw_fd(),
            request.as_ptr().cast(),
            2,
            libc::MSG_NOSIGNAL,
        ) != 2
        {
            return Err(io::Error::last_os_error());
        }
        let mut poll = libc::pollfd {
            fd: broker.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if libc::poll(&mut poll, 1, 1000) != 1 {
            return Err(io::Error::other("socket broker unavailable"));
        }
        let mut label = [0u8; 2];
        let mut ancillary = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: label.as_mut_ptr().cast(),
            iov_len: 2,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&ancillary);
        let received = libc::recvmsg(broker.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if received < 0 {
            return Err(io::Error::last_os_error());
        }
        // Own every delivered descriptor before validating the response.
        let mut sockets = Vec::new();
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*cmsg).cmsg_len.saturating_sub(libc::CMSG_LEN(0) as usize)) / 4;
                for index in 0..count {
                    let fd =
                        std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>().add(index));
                    sockets.push(UdpSocket::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
        if received != 2
            || label != request
            || msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
            || sockets.len() != 1
        {
            return Err(io::Error::other("invalid socket broker label"));
        }
        let socket = sockets.pop().unwrap();
        prepare(&socket)?;
        Ok(socket)
    }
}

struct Candidate {
    state: Control,
    seq: u32,
    attempts: u8,
    deadline: u128,
    socket: Option<UdpSocket>,
    stream: Stream,
}
fn advertisement(message: &Control) -> Control {
    let mut value = message.clone();
    if let Control::State { echo, .. } = &mut value {
        *echo = 0;
    }
    value
}

fn stale_candidate(candidate: &Control, incoming: &Control) -> bool {
    match (candidate, incoming) {
        (
            Control::State {
                instance: old_instance,
                epoch: old_epoch,
                ..
            },
            Control::State {
                instance, epoch, ..
            },
        ) => {
            instance == old_instance
                && (epoch < old_epoch
                    || epoch == old_epoch && advertisement(incoming) != *candidate)
        }
        _ => true,
    }
}

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
fn bind_rotation(
    address: SocketAddr,
    low: u16,
    high: u16,
    occupied: &[u16],
    provider: &dyn SocketProvider,
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
        match provider.prepare_port(addr) {
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
// Joining on drop prevents a late firewall task from racing supervisor cleanup.
struct Pending<T>(Option<std::thread::JoinHandle<T>>);
impl<T: std::marker::Send + 'static> Pending<T> {
    fn spawn(work: impl FnOnce() -> T + std::marker::Send + 'static) -> Self {
        Self(Some(std::thread::spawn(work)))
    }
    fn poll(&mut self) -> Option<T> {
        if self.0.as_ref()?.is_finished() {
            Some(self.0.take().unwrap().join().expect("port task panicked"))
        } else {
            None
        }
    }
}
impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            let _ = task.join();
        }
    }
}
struct Runtime {
    retire_tasks: BTreeMap<u16, Pending<io::Result<()>>>,
    retire_retry: BTreeMap<u16, Instant>,
    psk: [u8; 32],
    base: u16,
    peer: Option<SocketAddr>,
    client: Option<Client>,
    server: Option<Server>,
    sockets: BTreeMap<u16, UdpSocket>,
    streams: BTreeMap<u16, Stream>,
    drops: BTreeMap<u16, u32>,
    counts: Counts,
    started: Arc<dyn Clock>,
    trace_control: bool,
    provider: Arc<dyn SocketProvider>,
    events: Events,
    candidate: Option<Candidate>,
    connected: bool,
    test_blocked_path: Option<Path>,
    repair_started: Option<Duration>,
    retiring: std::collections::BTreeSet<u16>,
}
impl Runtime {
    fn trace(&self, mut fields: serde_json::Value) {
        if !self.trace_control {
            return;
        }
        use std::time::{SystemTime, UNIX_EPOCH};
        fields["event"] = "control_trace".into();
        fields["role"] = if self.client.is_some() {
            "client"
        } else {
            "server"
        }
        .into();
        fields["mono_us"] = (self.started.elapsed().as_micros() as u64).into();
        fields["unix_us"] = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64)
            .into();
        (self.events)(fields);
    }
    fn packet_trace(
        &self,
        stage: &str,
        port: u16,
        packet: &[u8],
        detail: impl FnOnce() -> serde_json::Value,
    ) {
        if !self.trace_control || packet.len() + 28 > MAX_CONTROL_IPV4 {
            return;
        }
        use sha2::{Digest, Sha256};
        let mut fields = detail();
        fields["stage"] = stage.into();
        fields["port"] = port.into();
        fields["bytes"] = packet.len().into();
        fields["packet"] = format!("{:x}", Sha256::digest(packet)).into();
        self.trace(fields);
    }
    fn ticks(&self) -> u32 {
        (self.started.elapsed().as_nanos() * 90000 / 1000000000) as u32
    }
    fn sync_client(&mut self, now: u64) {
        let c = self.client.as_ref().unwrap();
        self.streams.retain(|port, _| c.knows(*port, now));
        self.sockets.retain(|port, _| c.knows(*port, now));
        for p in c
            .rotation
            .iter()
            .chain(c.previous.iter().map(|(p, _)| p))
            .chain(c.candidate.iter())
        {
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
        let socket = self.sockets.get(&path.port);
        if socket.and_then(|s| s.send_to(packet, path.peer).ok()) == Some(packet.len()) {
            true
        } else {
            self.counts.socket_drops += 1;
            false
        }
    }
    fn renew_socket(&mut self, port: u16) -> io::Result<()> {
        let mut peer = self.peer.ok_or_else(|| io::Error::other("missing peer"))?;
        peer.set_port(port);
        let socket = self.provider.client_socket(peer)?;
        prepare(&socket)?;
        self.sockets.insert(port, socket);
        if let Some(stream) = self.streams.get(&port) {
            self.streams.insert(port, Stream::new(stream.keys.clone()));
        }
        self.counts.socket_renewals += 1;
        self.trace(serde_json::json!({"stage":"socket_renewed", "port":port}));
        Ok(())
    }
    fn repair_active(&mut self) {
        if self.repair_started.is_none() {
            (self.events)(
                serde_json::json!({"event":"repairing", "port":self.client.as_ref().unwrap().current}),
            );
        }
        self.repair_started
            .get_or_insert_with(|| self.started.elapsed());
        let c = self.client.as_mut().unwrap();
        c.cancel_pending();
        if self.candidate.is_none() {
            let Some(path) = c.rotation.clone() else {
                return;
            };
            let state = Control::State {
                session: c.session,
                echo: 0,
                instance: c.instance.unwrap(),
                epoch: c.epoch,
                port: path.port,
                secret: path.secret,
                base: self.base,
                grace: 60,
            };
            self.candidate = Some(Candidate {
                state,
                seq: 0,
                attempts: 0,
                deadline: 0,
                socket: None,
                stream: Stream::new(PortKeys::rotation(&self.psk, &path.secret, true)),
            });
            c.candidate = Some(path);
            self.counts.path_candidates += 1;
        }
        self.counts.active_repairs += 1;
        self.candidate_tick();
    }
    fn candidate_tick(&mut self) {
        let Some(candidate) = &self.candidate else {
            return;
        };
        if self.started.elapsed().as_millis() < candidate.deadline
            || self.client.as_ref().unwrap().pending_sequence().is_some()
        {
            return;
        }
        let Control::State {
            port,
            instance,
            epoch,
            ..
        } = candidate.state
        else {
            unreachable!()
        };
        if candidate.attempts == 0 || candidate.attempts >= 3 {
            let mut peer = self.peer.unwrap();
            peer.set_port(port);
            let fresh = self.provider.client_socket(peer).and_then(|socket| {
                prepare(&socket)?;
                Ok(socket)
            });
            let Ok(socket) = fresh else {
                self.counts.socket_drops += 1;
                self.candidate.as_mut().unwrap().deadline =
                    self.started.elapsed().as_millis() + 2000;
                return;
            };
            if self.candidate.as_ref().unwrap().attempts >= 3 {
                self.counts.candidate_retries += 1;
            }
            let candidate = self.candidate.as_mut().unwrap();
            candidate.attempts = 0;
            candidate.socket = Some(socket);
            candidate.stream = Stream::new(candidate.stream.keys.clone());
            self.counts.socket_renewals += 1;
            self.trace(
                serde_json::json!({"stage":"socket_renewed", "port":port, "candidate":true}),
            );
        }
        let Some(probe) = self.client.as_mut().unwrap().probe_message(instance, epoch) else {
            return;
        };
        let Control::Probe { seq, .. } = probe else {
            unreachable!()
        };
        let candidate = self.candidate.as_mut().unwrap();
        candidate.seq = seq;
        candidate.attempts += 1;
        candidate.deadline = self.started.elapsed().as_millis() + 1000;
        let mut peer = self.peer.unwrap();
        peer.set_port(port);
        self.control(Send {
            path: Path { port, peer },
            message: probe,
        });
    }
    fn client_state(
        &mut self,
        port: u16,
        msg: &Control,
        now: u64,
        from_candidate: bool,
    ) -> Result<Option<Control>, &'static str> {
        let Control::State {
            port: target,
            secret,
            instance,
            epoch,
            echo,
            ..
        } = *msg
        else {
            return Err("wrong_direction");
        };
        if let Some(candidate) = &self.candidate {
            if from_candidate
                && port == target
                && port
                    == self
                        .client
                        .as_ref()
                        .unwrap()
                        .candidate
                        .as_ref()
                        .unwrap()
                        .port
            {
                if echo == 0 || echo != candidate.seq || advertisement(msg) != candidate.state {
                    return Err("candidate_echo_not_pending");
                }
                let reply =
                    self.client
                        .as_mut()
                        .unwrap()
                        .accept_probe(port, msg, candidate.seq, now)?;
                let mut candidate = self.candidate.take().unwrap();
                self.sockets.insert(port, candidate.socket.take().unwrap());
                self.streams.insert(port, candidate.stream);
                // Registration PING still needs an authenticated STATE reply;
                // report repaired only after accept_state completes below.
                return Ok(reply);
            }
        }
        let c = self.client.as_ref().unwrap();
        let changes_path = c.instance != Some(instance) || epoch > c.epoch || c.current != target;
        if changes_path {
            if self
                .candidate
                .as_ref()
                .is_some_and(|p| stale_candidate(&p.state, msg))
            {
                return Err("stale_candidate_advertisement");
            }
            self.client
                .as_mut()
                .unwrap()
                .observe_state(port, msg, now)?;
            let state = advertisement(msg);
            if self.candidate.as_ref().is_none_or(|p| p.state != state) {
                self.client.as_mut().unwrap().candidate = Some(state::ClientPort {
                    port: target,
                    secret,
                });
                self.candidate = Some(Candidate {
                    state,
                    seq: 0,
                    attempts: 0,
                    deadline: 0,
                    socket: None,
                    stream: Stream::new(PortKeys::rotation(&self.psk, &secret, true)),
                });
                self.counts.path_candidates += 1;
                self.sync_client(now);
            }
            self.candidate_tick();
            return Ok(None);
        }
        let reply = self.client.as_mut().unwrap().accept_state(port, msg, now)?;
        if self.repair_started.take().is_some() {
            (self.events)(serde_json::json!({"event":"repaired", "port":port}));
        }
        if !self.connected && port == self.client.as_ref().unwrap().current && port != self.base {
            if self.provider.packet_ready().is_err() {
                return Err("tun_activation_failed");
            }
            self.connected = true;
            (self.events)(serde_json::json!({"event":"connected", "seconds":now}));
        }
        Ok(reply)
    }
    fn control(&mut self, action: Send) {
        // Explicit integration-only fault hook: lose announcements and their
        // retries while preserving solicited STATE, without decrypting in a
        // network proxy or confusing a later rotation with PING recovery.
        if matches!(action.message, Control::State { echo: 0, .. })
            && env::var_os("POC_TEST_DROP_ANNOUNCEMENTS_FILE")
                .is_some_and(|p| std::path::Path::new(&p).exists())
        {
            (self.events)(
                serde_json::json!({"event":"test_announcement_drop", "seconds":self.started.elapsed().as_secs()}),
            );
            return;
        }
        let metadata = control_metadata(&action.message);
        self.trace(serde_json::json!({"stage":"control_generated", "port":action.path.port, "control":metadata}));
        let ticks = self.ticks();
        let probing = matches!(action.message, Control::Probe { .. });
        let stream = if probing {
            self.candidate.as_mut().map(|c| &mut c.stream)
        } else {
            self.streams.get_mut(&action.path.port)
        };
        if let Some(s) = stream {
            let p =
                s.tx.seal_control(&s.keys.tx_control, &action.message, ticks, MAX_CONTROL_IPV4)
                    .unwrap();
            if matches!(action.message, Control::Ping { .. }) {
                self.counts.pings_sent += 1;
            }
            if matches!(action.message, Control::Probe { .. }) {
                self.counts.candidate_probes += 1;
            }
            let sent = if probing {
                let sent = self
                    .candidate
                    .as_ref()
                    .and_then(|c| c.socket.as_ref())
                    .and_then(|s| s.send_to(&p, action.path.peer).ok())
                    == Some(p.len());
                if !sent {
                    self.counts.socket_drops += 1;
                }
                sent
            } else {
                self.send(action.path, &p)
            };
            self.packet_trace(
                "send",
                action.path.port,
                &p,
                || serde_json::json!({"sent":sent, "control":metadata}),
            );
            if sent {
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
        self.retiring.insert(port);
        if self
            .retire_retry
            .get(&port)
            .is_some_and(|deadline| Instant::now() < *deadline)
        {
            return;
        }
        if !self.retire_tasks.contains_key(&port) {
            let provider = self.provider.clone();
            self.retire_tasks
                .insert(port, Pending::spawn(move || provider.retire_port(port)));
        }
        let Some(result) = self.retire_tasks.get_mut(&port).unwrap().poll() else {
            return;
        };
        self.retire_tasks.remove(&port);
        if let Err(error) = result {
            self.retire_retry
                .insert(port, Instant::now() + Duration::from_secs(1));
            (self.events)(
                serde_json::json!({"event":"degraded", "operation":"retire_port", "error":error.to_string()}),
            );
            return;
        }
        self.retiring.remove(&port);
        self.retire_retry.remove(&port);
        self.sockets.remove(&port);
        self.streams.remove(&port);
        self.drops.remove(&port);
    }
    fn incoming(
        &mut self,
        port: u16,
        source: SocketAddr,
        b: &[u8],
        now: u64,
        dev: &mut dyn PacketIo,
        from_candidate: bool,
    ) {
        if let Some(server) = &self.server {
            let inject = env::var_os("POC_TEST_DROP_ACTIVE_PATH_FILE")
                .is_some_and(|p| std::path::Path::new(&p).exists());
            let path = Path { port, peer: source };
            if inject
                && self.test_blocked_path.is_none()
                && server.sessions().values().any(|s| s.path == path)
            {
                self.test_blocked_path = Some(path);
                (self.events)(
                    serde_json::json!({"event":"test_active_path_blocked", "seconds":now}),
                );
            }
            if !inject {
                self.test_blocked_path = None;
            }
            if self.test_blocked_path == Some(path) {
                return;
            }
        }
        self.packet_trace(
            "receive",
            if self.client.is_some() {
                source.port()
            } else {
                port
            },
            b,
            || serde_json::json!({}),
        );
        if let Some(peer) = self.peer {
            if source.ip() != peer.ip() || !self.client.as_ref().unwrap().knows(source.port(), now)
            {
                self.packet_trace(
                    "decode",
                    source.port(),
                    b,
                    || serde_json::json!({"result":"foreign_or_unknown_source"}),
                );
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
            self.packet_trace(
                "decode",
                port,
                b,
                || serde_json::json!({"result":"missing_stream"}),
            );
            self.counts.invalid += 1;
            return;
        };
        match open_packet(&s.keys.rx_data, &s.keys.rx_control, b) {
            Some(Packet::Data(p)) => {
                if from_candidate {
                    self.counts.invalid += 1;
                    return;
                }
                self.packet_trace("decode", port, b, || serde_json::json!({"result":"data"}));
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
                self.packet_trace(
                    "decode",
                    port,
                    b,
                    || serde_json::json!({"result":"control", "control":control_metadata(&msg)}),
                );
                self.counts.control_rx += 1;
                self.counts.rx_wire += b.len() as u64;
                if let Some(c) = &mut self.client {
                    if !matches!(msg, Control::State { .. }) {
                        self.counts.control_rejected += 1;
                        return;
                    }
                    let instance = c.instance;
                    let pending = c.pending_sequence();
                    let result = self.client_state(port, &msg, now, from_candidate);
                    let outcome = match &result {
                        Ok(_) => "accepted",
                        Err(reason) => *reason,
                    };
                    let reply = result.ok().flatten();
                    let current = self.client.as_ref().unwrap().current;
                    self.packet_trace("state", port, b, || {
                        serde_json::json!({"result":outcome, "pending":pending,
                            "current":current, "control":control_metadata(&msg)})
                    });
                    let c = self.client.as_ref().unwrap();
                    if reply.is_some() {
                        self.counts.switches += 1;
                        if env::var_os("POC_TRACE_PATHS").is_some() {
                            (self.events)(serde_json::json!({"event":"path", "seconds":now,
                                "port":c.current, "epoch":c.epoch,
                                "instance_changed":instance != c.instance,
                                "solicited":matches!(msg, Control::State { echo, .. } if echo != 0)}));
                        }
                    }
                    self.sync_client(now);
                    if let Some(msg) = reply {
                        self.ping(msg);
                    }
                } else if matches!(msg, Control::Probe { .. }) {
                    if let Some(reply) =
                        self.server
                            .as_mut()
                            .unwrap()
                            .probe(Path { port, peer: source }, &msg, now)
                    {
                        self.packet_trace("probe", port, b, || serde_json::json!({"result":"accepted", "control":control_metadata(&msg)}));
                        if env::var_os("POC_TEST_DROP_PROBE_REPLIES_FILE")
                            .is_some_and(|p| std::path::Path::new(&p).exists())
                        {
                            (self.events)(
                                serde_json::json!({"event":"test_probe_reply_drop", "seconds":now}),
                            );
                        } else {
                            self.control(reply);
                        }
                    } else {
                        self.counts.control_rejected += 1;
                        self.packet_trace("probe", port, b, || serde_json::json!({"result":"rejected", "control":control_metadata(&msg)}));
                    }
                } else if matches!(msg, Control::Ping { .. }) {
                    let id = match msg {
                        Control::Ping { session, .. } => session,
                        _ => unreachable!(),
                    };
                    let old = self
                        .server
                        .as_ref()
                        .unwrap()
                        .sessions()
                        .get(&id)
                        .map(|s| s.path);
                    if let Some((reply, _)) =
                        self.server
                            .as_mut()
                            .unwrap()
                            .ping(Path { port, peer: source }, &msg, now)
                    {
                        self.packet_trace("ping", port, b,
                            || serde_json::json!({"result":"accepted", "control":control_metadata(&msg)}));
                        if env::var_os("POC_TRACE_PATHS").is_some() {
                            (self.events)(
                                serde_json::json!({"event":"registration", "seconds":now,
                                "session":id, "port":port, "new":old.is_none(),
                                "tuple_changed":old.is_some_and(|p| p.peer != source),
                                "same_port":old.is_some_and(|p| p.port == port)}),
                            );
                        }
                        self.control(reply);
                    } else {
                        self.packet_trace("ping", port, b,
                            || serde_json::json!({"result":"rejected", "control":control_metadata(&msg)}));
                        self.counts.control_rejected += 1;
                    }
                } else {
                    self.counts.control_rejected += 1;
                }
            }
            None => {
                self.packet_trace(
                    "decode",
                    port,
                    b,
                    || serde_json::json!({"result":"rejected"}),
                );
                // Encrypted malformed traffic cannot reliably be classified as
                // data vs control; count decoder failures in both categories.
                self.counts.invalid += 1;
                self.counts.control_rejected += 1;
            }
        }
    }
}
pub fn run(config: WorkerConfig, resources: Resources, running: Arc<AtomicBool>) -> io::Result<()> {
    let WorkerConfig {
        client,
        psk,
        inner,
        prefix,
        rotation_low: low,
        rotation_high: high,
        initial_rotation_port,
        rotation_seconds: mean,
        keepalive_seconds: keepalive,
    } = config;
    if prefix > 32 {
        return Err(io::Error::other("invalid IPv4 prefix"));
    }
    let mut next_rotation = if mean == 0 {
        u64::MAX
    } else {
        rotation_delay(mean).ok_or_else(|| io::Error::other("rotation interval out of range"))?
    };
    if keepalive_delay(keepalive).is_none() || keepalive > 45 {
        return Err(io::Error::other("keepalive interval out of range"));
    }
    let Resources {
        packet: mut dev,
        socket,
        provider,
        events,
        clock,
    } = resources;
    prepare(&socket)?;
    let peer = if client {
        Some(disconnect(&socket)?)
    } else {
        None
    };
    let address = socket.local_addr()?;
    let base = peer.unwrap_or(address).port();
    if base == 0
        || !client
            && (low == 0
                || high < low
                || u32::from(high) - u32::from(low) + 1 - u32::from((low..=high).contains(&base))
                    < 3)
    {
        return Err(io::Error::other("invalid base/rotation pool"));
    }
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
        started: clock,
        trace_control: env::var_os("POC_TRACE_CONTROL").is_some(),
        provider,
        events,
        candidate: None,
        connected: false,
        test_blocked_path: None,
        repair_started: None,
        retiring: Default::default(),
        retire_tasks: Default::default(),
        retire_retry: Default::default(),
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
        let (initial_low, initial_high) = match initial_rotation_port {
            Some(port) if port != base && (low..=high).contains(&port) => (port, port),
            Some(_) => return Err(io::Error::other("invalid initial rotation port")),
            None => (low, high),
        };
        let (port, socket) = bind_rotation(
            address,
            initial_low,
            initial_high,
            &[base],
            rt.provider.as_ref(),
        )?;
        rt.provider.commit_port(port)?;
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
    nonblocking(dev.as_raw_fd())?;
    if !client {
        rt.provider.packet_ready()?;
    }
    if let Some(c) = &mut rt.client {
        let ping = c.ping(0).unwrap();
        rt.ping(ping);
    }
    (rt.events)(serde_json::json!({"event":"ready","client":client,"protocol":PROTOCOL,"mtu":MTU}));
    let mut admission_ready = true;
    let mut rotation_task: Option<Pending<io::Result<(u16, UdpSocket)>>> = None;
    let mut reconcile_task: Option<Pending<io::Result<()>>> = None;
    let mut next_reconcile = Instant::now();
    let mut last = Instant::now();
    let mut b = [0; 65536];
    while running.load(Ordering::Relaxed) {
        let now = rt.started.elapsed().as_secs();
        if let Some(c) = &mut rt.client {
            let before = (c.pending_sequence(), c.current, c.fallbacks);
            let unanswered = c.pings_unanswered;
            let repairing = rt.repair_started.is_some_and(|start| {
                rt.started.elapsed().saturating_sub(start) < Duration::from_secs(8)
            }) && rt.candidate.is_some();
            let mut ping = if repairing { None } else { c.tick(now) };
            let repair = c.current != rt.base
                && c.pings_unanswered > unanswered
                && c.receive_idle(now) >= 2
                && rt.repair_started.is_none_or(|start| {
                    rt.started.elapsed().saturating_sub(start) < Duration::from_secs(8)
                });
            let after = (c.pending_sequence(), c.current, c.fallbacks);
            rt.counts.fallbacks = c.fallbacks;
            rt.counts.pings_unanswered = c.pings_unanswered;
            rt.counts.sessions = usize::from(c.instance.is_some());
            if ping.is_some() {
                c.set_keepalive_interval(keepalive_delay(keepalive).unwrap());
            }
            if before != after {
                rt.trace(
                    serde_json::json!({"stage":"timer", "pending_before":before.0,
                    "pending_after":after.0, "port_before":before.1, "port":after.1,
                    "fallback":after.2 > before.2}),
                );
            }
            if after.1 == rt.base
                && (after.2 > before.2 || ping.is_some() && after.2 > 0)
                && rt.renew_socket(rt.base).is_err()
            {
                rt.counts.socket_drops += 1;
            }
            if repair {
                rt.repair_active();
                ping = None;
            }
            rt.sync_client(now);
            if let Some(ping) = ping {
                rt.ping(ping);
            }
            rt.candidate_tick();
        } else {
            for port in rt.retiring.clone() {
                rt.close(port);
            }
            if now >= next_rotation
                && admission_ready
                && rt.retiring.is_empty()
                && rotation_task.is_none()
                && reconcile_task.is_none()
            {
                let provider = rt.provider.clone();
                let occupied = rt.server.as_ref().unwrap().ports();
                rotation_task = Some(Pending::spawn(move || {
                    let (port, socket) =
                        bind_rotation(address, low, high, &occupied, provider.as_ref())?;
                    if let Err(error) = provider.commit_port(port) {
                        let _ = provider.retire_port(port);
                        return Err(error);
                    }
                    Ok((port, socket))
                }));
            }
            if let Some(result) = (admission_ready && reconcile_task.is_none())
                .then(|| rotation_task.as_mut().and_then(Pending::poll))
                .flatten()
            {
                rotation_task = None;
                match result {
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
                            if env::var_os("POC_TRACE_PATHS").is_some() {
                                (rt.events)(
                                    serde_json::json!({"event":"rotation", "seconds":now, "port":port}),
                                );
                            }
                            rt.counts.announces_sent += actions.len() as u64;
                            for action in actions {
                                rt.control(action);
                            }
                        } else {
                            rt.sockets.insert(port, socket);
                            rt.close(port);
                        }
                    }
                    Err(error) => {
                        rt.counts.rotation_bind_failures += 1;
                        (rt.events)(
                            serde_json::json!({"event":"rotation_failed", "error":error.to_string()}),
                        );
                    }
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
        let candidate_index = polls.len();
        if let Some(socket) = rt.candidate.as_ref().and_then(|c| c.socket.as_ref()) {
            polls.push(libc::pollfd {
                fd: socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
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
                let Some(socket) = rt.sockets.get(port) else {
                    break;
                };
                match receive(socket, &mut b) {
                    Ok((n, source, drops)) => {
                        if let Some(d) = drops {
                            let prev = rt.drops.insert(*port, d).unwrap_or(0);
                            rt.counts.kernel_rx_drops += d.wrapping_sub(prev) as u64;
                        }
                        rt.incoming(*port, source, &b[..n], now, dev.as_mut(), false);
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
        if candidate_index + 1 < polls.len() && polls[candidate_index].revents & libc::POLLIN != 0 {
            for _ in 0..64 {
                let Some(candidate) = &rt.candidate else {
                    break;
                };
                let Control::State { port, .. } = candidate.state else {
                    unreachable!()
                };
                let Some(socket) = &candidate.socket else {
                    break;
                };
                match receive(socket, &mut b) {
                    Ok((n, source, _)) => {
                        rt.incoming(port, source, &b[..n], now, dev.as_mut(), true)
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
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
        if reconcile_task.is_none() && Instant::now() >= next_reconcile {
            let provider = rt.provider.clone();
            reconcile_task = Some(Pending::spawn(move || provider.reconcile_ports()));
        }
        if let Some(result) = reconcile_task.as_mut().and_then(Pending::poll) {
            reconcile_task = None;
            let was_ready = admission_ready;
            admission_ready = result.is_ok();
            next_reconcile = Instant::now() + Duration::from_secs(1);
            if !was_ready && admission_ready {
                (rt.events)(
                    serde_json::json!({"event":"admission_restored", "connected":rt.connected}),
                );
            }
            if let Err(error) = result {
                (rt.events)(
                    serde_json::json!({"event":"degraded", "operation":"reconcile_ports", "error":error.to_string()}),
                );
            }
        }
        if last.elapsed() >= Duration::from_secs(5) {
            rt.counts.report(
                rt.started.elapsed().as_secs_f64(),
                false,
                rt.events.as_ref(),
            );
            last = Instant::now();
        }
    }
    rt.counts
        .report(rt.started.elapsed().as_secs_f64(), true, rt.events.as_ref());
    Ok(())
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn reordered_advertisement_cannot_replace_a_newer_pending_candidate() {
        let candidate = Control::State {
            session: 1,
            echo: 0,
            instance: 7,
            epoch: 3,
            port: 4003,
            secret: [3; 32],
            base: 4000,
            grace: 60,
        };
        let mut incoming = candidate.clone();
        if let Control::State { echo, .. } = &mut incoming {
            *echo = 9;
        }
        assert!(!stale_candidate(&candidate, &incoming));
        if let Control::State { epoch, .. } = &mut incoming {
            *epoch = 2;
        }
        assert!(stale_candidate(&candidate, &incoming));
        if let Control::State { epoch, secret, .. } = &mut incoming {
            *epoch = 3;
            *secret = [9; 32];
        }
        assert!(stale_candidate(&candidate, &incoming));
        if let Control::State { epoch, .. } = &mut incoming {
            *epoch = 4;
        }
        assert!(!stale_candidate(&candidate, &incoming));
    }

    fn broker_reply(
        wrong_label: bool,
        descriptors: usize,
    ) -> (fs::File, std::thread::JoinHandle<Vec<SocketAddr>>) {
        use std::os::{fd::IntoRawFd, unix::net::UnixDatagram};
        let (client, parent) = UnixDatagram::pair().unwrap();
        let task = std::thread::spawn(move || {
            let mut label = [0u8; 2];
            assert_eq!(parent.recv(&mut label).unwrap(), 2);
            if wrong_label {
                label[0] ^= 1;
            }
            let sockets: Vec<_> = (0..descriptors)
                .map(|_| UdpSocket::bind("127.0.0.1:0").unwrap())
                .collect();
            let addresses = sockets.iter().map(|s| s.local_addr().unwrap()).collect();
            let rights: Vec<_> = sockets.iter().map(AsRawFd::as_raw_fd).collect();
            unsafe {
                let mut ancillary = [0usize; 8];
                let mut iov = libc::iovec {
                    iov_base: label.as_mut_ptr().cast(),
                    iov_len: 2,
                };
                let mut msg: libc::msghdr = std::mem::zeroed();
                msg.msg_iov = &mut iov;
                msg.msg_iovlen = 1;
                msg.msg_control = ancillary.as_mut_ptr().cast();
                msg.msg_controllen = libc::CMSG_SPACE((rights.len() * 4) as _) as _;
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN((rights.len() * 4) as _) as _;
                std::ptr::copy_nonoverlapping(
                    rights.as_ptr(),
                    libc::CMSG_DATA(cmsg).cast(),
                    rights.len(),
                );
                assert_eq!(libc::sendmsg(parent.as_raw_fd(), &msg, 0), 2);
            }
            addresses
        });
        (unsafe { fs::File::from_raw_fd(client.into_raw_fd()) }, task)
    }

    #[test]
    fn broker_transfers_a_usable_host_socket() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (channel, parent) = broker_reply(false, 1);
        let socket = broker_socket(&channel, 4001).unwrap();
        let address = parent.join().unwrap()[0];
        assert_eq!(socket.local_addr().unwrap(), address);
        socket
            .send_to(b"candidate", receiver.local_addr().unwrap())
            .unwrap();
        let mut bytes = [0; 16];
        let (length, source) = receiver.recv_from(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"candidate");
        assert_eq!(source, address);
    }

    #[test]
    fn broker_rejects_mislabeled_or_extra_rights_without_leaking_sockets() {
        for (wrong_label, descriptors) in [(true, 1), (false, 2)] {
            let (channel, parent) = broker_reply(wrong_label, descriptors);
            assert!(broker_socket(&channel, 4001).is_err());
            for address in parent.join().unwrap() {
                UdpSocket::bind(address).unwrap();
            }
        }
    }

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
