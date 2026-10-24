#[cfg(target_os = "linux")]
mod linux {
    use rtp_l3_poc::*;
    use std::{
        env, fs,
        io::{self, Read, Write},
        net::{SocketAddr, UdpSocket},
        os::fd::{AsRawFd, FromRawFd},
        process::Command,
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };
    static RUN: AtomicBool = AtomicBool::new(true);
    extern "C" fn stop(_: libc::c_int) {
        RUN.store(false, Ordering::Relaxed);
    }
    fn ip(args: &[&str]) -> io::Result<()> {
        if Command::new("ip").args(args).status()?.success() {
            Ok(())
        } else {
            Err(io::Error::other("ip setup failed"))
        }
    }
    fn tun(name: &str, address: &str) -> io::Result<fs::File> {
        if name.len() > 15 || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(io::Error::other("invalid interface name"));
        }
        let f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/net/tun")?;
        // Linux ifreq: 16-byte name followed by flags, padding to 40 bytes on x86_64.
        let mut req = [0u8; 40];
        req[..name.len()].copy_from_slice(name.as_bytes());
        req[16..18].copy_from_slice(&(0x1001u16).to_ne_bytes());
        if unsafe { libc::ioctl(f.as_raw_fd(), 0x400454ca, req.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        ip(&["addr", "add", address, "dev", name])?;
        ip(&["link", "set", "dev", name, "mtu", "1200", "up"])?;
        Ok(f)
    }
    #[derive(Default)]
    struct Counts {
        tx_packets: u64,
        rx_packets: u64,
        tx_plain: u64,
        rx_plain: u64,
        tx_wire: u64,
        rx_wire: u64,
        invalid: u64,
        tun_drops: u64,
        socket_drops: u64,
        kernel_rx_drops: u32,
    }
    impl Counts {
        fn report(&self, elapsed: f64, final_report: bool, x: bool) {
            // V2 has no replay detection: its counter only covers malformed,
            // non-IPv4-after-XOR or foreign-source datagrams.
            let mut j = serde_json::json!({"event":if final_report{"final"}else{"stats"},"protocol":if x{"xchacha20-v2"}else{"v1"},"seconds":elapsed,"tx_packets":self.tx_packets,"rx_packets":self.rx_packets,"tx_plain_bytes":self.tx_plain,"rx_plain_bytes":self.rx_plain,"tx_udp_payload_bytes":self.tx_wire,"rx_udp_payload_bytes":self.rx_wire,"tun_write_drops":self.tun_drops,"socket_send_drops":self.socket_drops,"socket_receive_drops":self.kernel_rx_drops});
            j[if x { "rejected" } else { "invalid_or_replay" }] = self.invalid.into();
            eprintln!("{j}");
        }
    }
    fn receive(socket: &UdpSocket, b: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<u32>)> {
        unsafe {
            let mut addr: libc::sockaddr_in = std::mem::zeroed();
            let mut control = [0usize; 8];
            let mut iov = libc::iovec {
                iov_base: b.as_mut_ptr() as *mut _,
                iov_len: b.len(),
            };
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_name = &mut addr as *mut _ as *mut _;
            msg.msg_namelen = std::mem::size_of_val(&addr) as _;
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr() as *mut _;
            msg.msg_controllen = std::mem::size_of_val(&control);
            let n = libc::recvmsg(socket.as_raw_fd(), &mut msg, 0);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if msg.msg_flags & libc::MSG_TRUNC != 0 || addr.sin_family != libc::AF_INET as u16 {
                return Err(io::Error::other("truncated/non-IPv4 UDP"));
            }
            let mut drops = None;
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET
                    && (*cmsg).cmsg_type == libc::SO_RXQ_OVFL
                    && (*cmsg).cmsg_len >= libc::CMSG_LEN(4) as usize
                {
                    drops = Some(std::ptr::read_unaligned(libc::CMSG_DATA(cmsg) as *const u32));
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
            Ok((
                n as usize,
                SocketAddr::new(
                    std::net::Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)).into(),
                    u16::from_be(addr.sin_port),
                ),
                drops,
            ))
        }
    }
    fn key(path: &str) -> io::Result<[u8; 32]> {
        use std::os::unix::fs::PermissionsExt;
        let m = fs::metadata(path)?;
        if m.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other("key file must have private permissions"));
        }
        fs::read(path)?
            .try_into()
            .map_err(|_| io::Error::other("key must be exactly 32 bytes"))
    }
    fn connect(socket: &UdpSocket, key: &[u8; 32]) -> io::Result<Session> {
        let hello = Control {
            kind: HELLO,
            c: rand::random(),
            s: [0; 32],
        };
        let mut outgoing = hello.clone();
        let mut b = [0; 2048];
        let start = Instant::now();
        socket.set_read_timeout(Some(Duration::from_millis(400)))?;
        while start.elapsed() < Duration::from_secs(12) && RUN.load(Ordering::Relaxed) {
            socket.send(&outgoing.encode(key))?;
            if let Ok(n) = socket.recv(&mut b) {
                if let Some(m) = Control::decode(&b[..n], key) {
                    if m.c != hello.c {
                        continue;
                    }
                    if m.kind == CHALLENGE && outgoing.kind == HELLO {
                        outgoing = m.with_kind(FINISH);
                    } else if m.kind == READY && outgoing.kind == FINISH && m.s == outgoing.s {
                        socket.set_read_timeout(None)?;
                        return Ok(Session::new(key, &m.c, &m.s, true));
                    }
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "authenticated handshake timed out",
        ))
    }
    pub fn main() -> io::Result<()> {
        // mode key-file interface cidr [inherited-fd | listen-address] [peer]
        let a: Vec<String> = env::args().collect();
        let (client, x) = match a.get(1).map(String::as_str) {
            Some("client") => (true, false),
            Some("server") => (false, false),
            Some("client-x") => (true, true),
            Some("server-x") => (false, true),
            _ => return Err(io::Error::other("invalid mode")),
        };
        if a.len() != if !client && x { 7 } else { 6 } {
            return Err(io::Error::other(
                "usage: rtp-l3-poc client|client-x KEY_FILE INTERFACE CIDR FD | server KEY_FILE INTERFACE CIDR LISTEN | server-x KEY_FILE INTERFACE CIDR LISTEN PEER",
            ));
        }
        let k = key(&a[2])?;
        unsafe {
            libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        }
        let socket = if client {
            let fd: i32 = a[5]
                .parse()
                .map_err(|_| io::Error::other("invalid inherited FD"))?;
            if fd < 3 {
                return Err(io::Error::other("FD must be >=3"));
            }
            unsafe { UdpSocket::from_raw_fd(fd) }
        } else {
            UdpSocket::bind(&a[5])?
        };
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
        let mut session = if client && !x {
            Some(connect(&socket, &k)?)
        } else {
            None
        };
        // V2 needs no exchange: keys come from the PSK and the server's return
        // address is configured out of band.
        let mut xtx = None;
        let mut xrx = [0; 32];
        let mut peer: Option<SocketAddr> = None;
        if x {
            let (t, r) = xchacha::keys(&k, client);
            xtx = Some(xchacha::Tx::new(t));
            xrx = r;
            if !client {
                peer = Some(
                    a[6].parse()
                        .map_err(|_| io::Error::other("invalid PEER address"))?,
                );
            }
        }
        let mut dev = tun(&a[3], &a[4])?;
        for fd in [socket.as_raw_fd(), dev.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        if client {
            ip(&["link", "set", "lo", "up"])?;
        }
        eprintln!(
            "{}",
            serde_json::json!({"event":"ready","client":client,"protocol":if x{"xchacha20-v2"}else{"v1"},"mtu":MTU,"socket_receive_buffer":({let mut v:libc::c_int=0;let mut l:libc::socklen_t=4;unsafe{libc::getsockopt(socket.as_raw_fd(),libc::SOL_SOCKET,libc::SO_RCVBUF,&mut v as *mut _ as *mut _,&mut l);}v})})
        );
        let mut handshake = ServerHandshake::default();
        let mut counters = Counts::default();
        let started = Instant::now();
        let mut last = Instant::now();
        let mut b = [0u8; 65536];
        while RUN.load(Ordering::Relaxed) {
            let mut polls = [
                libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: dev.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let r = unsafe { libc::poll(polls.as_mut_ptr(), 2, 250) };
            if r < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error());
            }
            if polls[0].revents & libc::POLLIN != 0 {
                for _ in 0..64 {
                    match receive(&socket, &mut b) {
                        Ok((n, source, drops)) => {
                            if let Some(d) = drops {
                                counters.kernel_rx_drops = d;
                            }
                            if !x && b[..n].starts_with(b"R3P1") {
                                if !client {
                                    if let Some(m) = Control::decode(&b[..n], &k) {
                                        if let Some((response, install)) =
                                            handshake.receive(m, source.to_string(), Instant::now())
                                        {
                                            if install {
                                                session = Some(Session::new(
                                                    &k,
                                                    &response.c,
                                                    &response.s,
                                                    false,
                                                ));
                                                peer = Some(source);
                                            }
                                            let _ = socket.send_to(&response.encode(&k), source);
                                        }
                                    } else {
                                        counters.invalid += 1;
                                    }
                                }
                                continue;
                            }
                            if !client && peer != Some(source) {
                                counters.invalid += 1;
                                continue;
                            }
                            let opened = if x {
                                Some(xchacha::open(&xrx, &b[..n]))
                            } else {
                                session.as_mut().map(|s| s.rx.open(&b[..n]))
                            };
                            if let Some(Some(p)) = opened {
                                counters.rx_packets += 1;
                                counters.rx_plain += p.len() as u64;
                                counters.rx_wire += n as u64;
                                if dev.write(&p).ok() != Some(p.len()) {
                                    counters.tun_drops += 1;
                                }
                            } else {
                                counters.invalid += 1;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
            }
            if polls[1].revents & libc::POLLIN != 0 {
                for _ in 0..64 {
                    match dev.read(&mut b) {
                        Ok(n) => {
                            let ticks = (started.elapsed().as_nanos() * 90000 / 1000000000) as u32;
                            let sealed = if let Some(t) = &mut xtx {
                                Some(t.seal(&b[..n], ticks))
                            } else {
                                session.as_mut().map(|s| s.tx.seal(&b[..n], ticks))
                            };
                            if let Some(sealed) = sealed {
                                if let Some(p) = sealed {
                                    let sent = if client {
                                        socket.send(&p)
                                    } else if let Some(addr) = peer {
                                        socket.send_to(&p, addr)
                                    } else {
                                        continue;
                                    };
                                    if sent.ok() == Some(p.len()) {
                                        counters.tx_packets += 1;
                                        counters.tx_plain += n as u64;
                                        counters.tx_wire += p.len() as u64;
                                    } else {
                                        counters.socket_drops += 1;
                                    }
                                } else {
                                    counters.tun_drops += 1;
                                }
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
            }
            if last.elapsed() >= Duration::from_secs(5) {
                counters.report(started.elapsed().as_secs_f64(), false, x);
                last = Instant::now();
            }
        }
        counters.report(started.elapsed().as_secs_f64(), true, x);
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux runtime required; protocol library tests are portable");
}
