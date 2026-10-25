//! Bounded, payload-opaque static UDP forwarding. No application protocol state.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::{SocketAddr, UdpSocket},
    os::fd::AsRawFd,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub id: String,
    pub generation: String,
    pub listen: SocketAddr,
    pub upstream: SocketAddr,
    pub max_flows: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mappings: Vec<Mapping>,
    pub max_flows: usize,
    pub max_source_flows: usize,
    pub max_payload: usize,
    pub socket_buffer: usize,
    pub idle_seconds: u64,
    /// Each ceiling applies independently to ingress + upstream responses.
    pub packets_per_second: u64,
    pub bytes_per_second: u64,
    pub mapping_packets_per_second: u64,
    pub mapping_bytes_per_second: u64,
    pub source_packets_per_second: u64,
    pub source_bytes_per_second: u64,
}
impl Config {
    pub fn validate(&self) -> io::Result<()> {
        let mut ids = HashSet::new();
        let mut addresses = HashSet::new();
        if self.mappings.is_empty()
            || self.mappings.len() > 64
            || self.max_flows == 0
            || self.max_flows > 16384
            || self.max_source_flows == 0
            || self.max_source_flows > self.max_flows
            || !(1..=65507).contains(&self.max_payload)
            || !(4096..=262144).contains(&self.socket_buffer)
            || self.idle_seconds == 0
            || self.idle_seconds > 86400
            || [
                self.packets_per_second,
                self.bytes_per_second,
                self.mapping_packets_per_second,
                self.mapping_bytes_per_second,
                self.source_packets_per_second,
                self.source_bytes_per_second,
            ]
            .contains(&0)
        {
            return Err(io::Error::other("invalid bounded relay limits"));
        }
        for m in &self.mappings {
            if m.id.is_empty()
                || m.id.len() > 128
                || m.generation.is_empty()
                || m.generation.len() > 128
                || !ids.insert(&m.id)
                || !addresses.insert(m.listen)
                || m.listen.port() == 0
                || m.upstream.port() == 0
                || m.upstream.ip().is_unspecified()
                || m.upstream.ip().is_multicast()
                || m.max_flows == 0
                || m.max_flows > self.max_flows
            {
                return Err(io::Error::other("invalid or conflicting mapping"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Default, Debug, Serialize)]
pub struct Counters {
    pub received: u64,
    pub sent: u64,
    pub bytes_sent: u64,
    pub oversized: u64,
    pub quota_drops: u64,
    pub rate_drops: u64,
    pub io_errors: u64,
    pub socket_receive_drops: u64,
    pub expired: u64,
    pub created: u64,
    pub active: usize,
}
#[derive(Default)]
struct Budget {
    packets: u64,
    bytes: u64,
}
impl Budget {
    fn allows(&self, n: usize, p: u64, b: u64) -> bool {
        self.packets < p && self.bytes.saturating_add(n as u64) <= b
    }
    fn charge(&mut self, n: usize) {
        self.packets += 1;
        self.bytes += n as u64;
    }
}
struct Source {
    budget: Budget,
    flows: usize,
}
struct Flow {
    socket: UdpSocket,
    overflow: u32,
    last: Instant,
}
struct Listener {
    config: Mapping,
    socket: UdpSocket,
    overflow: u32,
    flows: HashMap<SocketAddr, Flow>,
    budget: Budget,
    counters: Counters,
}
pub struct Relay {
    config: Config,
    listeners: Vec<Listener>,
    sources: HashMap<std::net::IpAddr, Source>,
    budget: Budget,
    window: Instant,
    buffer: Vec<u8>,
    pub counters: Counters,
}
fn socket(bind: SocketAddr, buffer: usize) -> io::Result<UdpSocket> {
    let s = UdpSocket::bind(bind)?;
    s.set_nonblocking(true)?;
    let size = buffer as libc::c_int;
    for opt in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
        if unsafe {
            libc::setsockopt(
                s.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &size as *const _ as *const _,
                std::mem::size_of_val(&size) as _,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let enable: libc::c_int = 1;
    if unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RXQ_OVFL,
            &enable as *const _ as *const _,
            std::mem::size_of_val(&enable) as _,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(s)
}
/// MSG_TRUNC returns the original length, ensuring oversized datagrams never
/// become accepted prefixes. SO_RXQ_OVFL reports kernel receive-queue drops.
fn receive(
    s: &UdpSocket,
    b: &mut [u8],
    overflow: &mut u32,
) -> io::Result<Option<(usize, SocketAddr, u64)>> {
    unsafe {
        let mut address: libc::sockaddr_storage = std::mem::zeroed();
        let mut control = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: b.as_mut_ptr() as _,
            iov_len: b.len(),
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_name = &mut address as *mut _ as _;
        msg.msg_namelen = std::mem::size_of_val(&address) as _;
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr() as _;
        msg.msg_controllen = std::mem::size_of_val(&control);
        let n = libc::recvmsg(s.as_raw_fd(), &mut msg, libc::MSG_TRUNC);
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(e)
            };
        }
        let peer = match address.ss_family as i32 {
            libc::AF_INET => {
                let a = &*(&address as *const _ as *const libc::sockaddr_in);
                SocketAddr::new(
                    std::net::Ipv4Addr::from(a.sin_addr.s_addr.to_ne_bytes()).into(),
                    u16::from_be(a.sin_port),
                )
            }
            libc::AF_INET6 => {
                let a = &*(&address as *const _ as *const libc::sockaddr_in6);
                SocketAddr::V6(std::net::SocketAddrV6::new(
                    std::net::Ipv6Addr::from(a.sin6_addr.s6_addr),
                    u16::from_be(a.sin6_port),
                    a.sin6_flowinfo,
                    a.sin6_scope_id,
                ))
            }
            _ => return Err(io::Error::other("unsupported peer address")),
        };
        let mut drops = 0;
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET
                && (*cmsg).cmsg_type == libc::SO_RXQ_OVFL
                && (*cmsg).cmsg_len >= libc::CMSG_LEN(4) as usize
            {
                let current = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const u32);
                drops = u64::from(current.wrapping_sub(*overflow));
                *overflow = current;
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
        Ok(Some((n as usize, peer, drops)))
    }
}

impl Relay {
    /// Atomic startup: partial bind failure drops every previously opened listener.
    /// Firewall admission is explicitly administrator-managed in this static CLI.
    pub fn bind(config: Config) -> io::Result<Self> {
        config.validate()?;
        let mut listeners = Vec::new();
        for m in &config.mappings {
            listeners.push(Listener {
                config: m.clone(),
                socket: socket(m.listen, config.socket_buffer)?,
                overflow: 0,
                flows: HashMap::new(),
                budget: Budget::default(),
                counters: Counters::default(),
            });
        }
        Ok(Self {
            buffer: vec![0; config.max_payload + 1],
            config,
            listeners,
            sources: HashMap::new(),
            budget: Budget::default(),
            window: Instant::now(),
            counters: Counters::default(),
        })
    }
    pub fn inventory(&self) -> serde_json::Value {
        serde_json::json!({"counters":self.counters,"mappings":self.listeners.iter().map(|m|serde_json::json!({"id":m.config.id,"generation":m.config.generation,"flows":m.flows.len(),"counters":m.counters})).collect::<Vec<_>>()})
    }
    pub fn tick(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.window) >= Duration::from_secs(1) {
            self.window = now;
            self.sources.retain(|_, s| s.flows > 0);
            self.budget = Budget::default();
            for l in &mut self.listeners {
                l.budget = Budget::default();
            }
            for s in self.sources.values_mut() {
                s.budget = Budget::default();
            }
        }
        for l in &mut self.listeners {
            let old = l.flows.len();
            l.flows.retain(|peer, f| {
                if now.duration_since(f.last) >= Duration::from_secs(self.config.idle_seconds) {
                    self.sources.get_mut(&peer.ip()).unwrap().flows -= 1;
                    false
                } else {
                    true
                }
            });
            let removed = old - l.flows.len();
            self.counters.expired += removed as u64;
            l.counters.expired += removed as u64;
            self.counters.active -= removed;
            l.counters.active -= removed;
            for _ in 0..32 {
                let (n, peer, drops) = match receive(&l.socket, &mut self.buffer, &mut l.overflow) {
                    Ok(Some(v)) => v,
                    Ok(None) => break,
                    Err(_) => {
                        self.counters.io_errors += 1;
                        l.counters.io_errors += 1;
                        break;
                    }
                };
                self.counters.socket_receive_drops += drops;
                l.counters.socket_receive_drops += drops;
                self.counters.received += 1;
                l.counters.received += 1;
                if n > self.config.max_payload {
                    self.counters.oversized += 1;
                    l.counters.oversized += 1;
                    continue;
                }
                let exists = l.flows.contains_key(&peer);
                let sf = self.sources.get(&peer.ip()).map_or(0, |s| s.flows);
                if !exists
                    && (self.counters.active >= self.config.max_flows
                        || l.flows.len() >= l.config.max_flows
                        || sf >= self.config.max_source_flows)
                {
                    self.counters.quota_drops += 1;
                    l.counters.quota_drops += 1;
                    continue;
                }
                if !self.sources.contains_key(&peer.ip()) {
                    if self.sources.len() >= self.config.max_flows {
                        self.counters.quota_drops += 1;
                        l.counters.quota_drops += 1;
                        continue;
                    }
                    self.sources.insert(
                        peer.ip(),
                        Source {
                            budget: Budget::default(),
                            flows: 0,
                        },
                    );
                }
                if !charge(
                    &self.config,
                    &mut self.budget,
                    &mut l.budget,
                    self.sources.get_mut(&peer.ip()).map(|s| &mut s.budget),
                    n,
                ) {
                    self.counters.rate_drops += 1;
                    l.counters.rate_drops += 1;
                    continue;
                }
                if !exists {
                    let bind = if l.config.upstream.is_ipv4() {
                        "0.0.0.0:0"
                    } else {
                        "[::]:0"
                    }
                    .parse()
                    .unwrap();
                    match socket(bind, self.config.socket_buffer).and_then(|s| {
                        s.connect(l.config.upstream)?;
                        Ok(s)
                    }) {
                        Ok(s) => {
                            l.flows.insert(
                                peer,
                                Flow {
                                    socket: s,
                                    overflow: 0,
                                    last: now,
                                },
                            );
                            self.sources.get_mut(&peer.ip()).unwrap().flows += 1;
                            self.counters.active += 1;
                            l.counters.active += 1;
                            self.counters.created += 1;
                            l.counters.created += 1;
                        }
                        Err(_) => {
                            self.counters.io_errors += 1;
                            l.counters.io_errors += 1;
                            continue;
                        }
                    }
                }
                let f = l.flows.get_mut(&peer).unwrap();
                f.last = now;
                match f.socket.send(&self.buffer[..n]) {
                    Ok(v) if v == n => {
                        self.counters.sent += 1;
                        self.counters.bytes_sent += n as u64;
                        l.counters.sent += 1;
                        l.counters.bytes_sent += n as u64;
                    }
                    _ => {
                        self.counters.io_errors += 1;
                        l.counters.io_errors += 1;
                    }
                }
            }
            for (peer, f) in &mut l.flows {
                for _ in 0..4 {
                    let (n, _, drops) = match receive(&f.socket, &mut self.buffer, &mut f.overflow)
                    {
                        Ok(Some(v)) => v,
                        Ok(None) => break,
                        Err(_) => {
                            self.counters.io_errors += 1;
                            l.counters.io_errors += 1;
                            break;
                        }
                    };
                    self.counters.socket_receive_drops += drops;
                    l.counters.socket_receive_drops += drops;
                    self.counters.received += 1;
                    l.counters.received += 1;
                    if n > self.config.max_payload {
                        self.counters.oversized += 1;
                        l.counters.oversized += 1;
                        continue;
                    }
                    if !charge(
                        &self.config,
                        &mut self.budget,
                        &mut l.budget,
                        self.sources.get_mut(&peer.ip()).map(|s| &mut s.budget),
                        n,
                    ) {
                        self.counters.rate_drops += 1;
                        l.counters.rate_drops += 1;
                        continue;
                    }
                    f.last = now;
                    match l.socket.send_to(&self.buffer[..n], peer) {
                        Ok(v) if v == n => {
                            self.counters.sent += 1;
                            self.counters.bytes_sent += n as u64;
                            l.counters.sent += 1;
                            l.counters.bytes_sent += n as u64;
                        }
                        _ => {
                            self.counters.io_errors += 1;
                            l.counters.io_errors += 1;
                        }
                    }
                }
            }
        }
    }
    pub fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            self.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
fn charge(c: &Config, g: &mut Budget, m: &mut Budget, s: Option<&mut Budget>, n: usize) -> bool {
    if !g.allows(n, c.packets_per_second, c.bytes_per_second)
        || !m.allows(n, c.mapping_packets_per_second, c.mapping_bytes_per_second)
        || s.as_ref()
            .is_some_and(|s| !s.allows(n, c.source_packets_per_second, c.source_bytes_per_second))
        || n as u64 > c.source_bytes_per_second
    {
        return false;
    }
    g.charge(n);
    m.charge(n);
    if let Some(s) = s {
        s.charge(n);
    }
    true
}
