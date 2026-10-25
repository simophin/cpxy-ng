#[cfg(target_os = "linux")]
mod linux {
    use std::{
        env, fs,
        io::{self, Write},
        net::{Ipv4Addr, SocketAddr, UdpSocket},
        os::fd::{AsRawFd, FromRawFd},
        process::Command,
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
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
    fn tun(name: &str, address: &str, up: bool) -> io::Result<fs::File> {
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
        ip(&["link", "set", "dev", name, "mtu", "1200"])?;
        if up {
            ip(&["link", "set", "dev", name, "up"])?;
        }
        Ok(f)
    }
    fn report_event(value: serde_json::Value) {
        // Serialize before taking stderr: shell job notifications can share this
        // file during namespace shutdown. Emit each JSON line in one write.
        let line = format!("{value}\n");
        let _ = io::stderr().lock().write_all(line.as_bytes());
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
    use rtp_l3::linux::{self, Resources, SocketProvider, WorkerConfig};
    use std::sync::{Arc, Mutex};
    struct Legacy {
        interface: String,
        broker: Option<Mutex<fs::File>>,
    }
    impl SocketProvider for Legacy {
        fn prepare_port(&self, address: SocketAddr) -> io::Result<UdpSocket> {
            UdpSocket::bind(address)
        }
        fn commit_port(&self, _: u16) -> io::Result<()> {
            Ok(())
        }
        fn retire_port(&self, _: u16) -> io::Result<()> {
            Ok(())
        }
        fn reconcile_ports(&self) -> io::Result<()> {
            Ok(())
        }
        fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket> {
            linux::broker_socket(
                &self
                    .broker
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing broker"))?
                    .lock()
                    .unwrap(),
                peer.port(),
            )
        }
        fn packet_ready(&self) -> io::Result<()> {
            ip(&["link", "set", "dev", &self.interface, "up"])
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
        let (inner, prefix) = a[4]
            .split_once('/')
            .ok_or_else(|| io::Error::other("invalid CIDR"))?;
        let inner: Ipv4Addr = inner.parse().map_err(io::Error::other)?;
        let prefix: u8 = prefix.parse().map_err(io::Error::other)?;
        if prefix > 32 {
            return Err(io::Error::other("invalid prefix"));
        }
        let (low, high) = if client {
            (0, 0)
        } else {
            let (low, high) = a[6]
                .split_once('-')
                .ok_or_else(|| io::Error::other("invalid range"))?;
            let low: u16 = low.parse().map_err(io::Error::other)?;
            let high: u16 = high.parse().map_err(io::Error::other)?;
            if low == 0 || high < low || u32::from(high) - u32::from(low) < 3 {
                return Err(io::Error::other("invalid range"));
            }
            (low, high)
        };
        let socket = if client {
            let fd: i32 = a[5].parse().map_err(io::Error::other)?;
            if fd < 3 {
                return Err(io::Error::other("invalid inherited FD"));
            }
            unsafe { UdpSocket::from_raw_fd(fd) }
        } else {
            UdpSocket::bind(&a[5])?
        };
        let broker = if client {
            let fd: i32 = env::var("POC_SOCKET_BROKER_FD")
                .map_err(io::Error::other)?
                .parse()
                .map_err(io::Error::other)?;
            if fd < 3 {
                return Err(io::Error::other("invalid broker FD"));
            }
            Some(Mutex::new(unsafe { fs::File::from_raw_fd(fd) }))
        } else {
            None
        };
        let packet = tun(&a[3], &a[4], !client)?;
        unsafe {
            libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        }
        let running = Arc::new(AtomicBool::new(true));
        let watch = running.clone();
        std::thread::spawn(move || {
            while RUN.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            watch.store(false, Ordering::Relaxed);
        });
        linux::run(
            WorkerConfig {
                client,
                psk: key(&a[2])?,
                inner,
                prefix,
                rotation_low: low,
                rotation_high: high,
                rotation_seconds: a
                    .get(7)
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(io::Error::other)?
                    .unwrap_or(300),
                keepalive_seconds: env::var("POC_KEEPALIVE_SECS")
                    .ok()
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(io::Error::other)?
                    .unwrap_or(15),
            },
            Resources {
                packet: Box::new(packet),
                clock: Arc::new(linux::MonotonicClock::default()),
                socket,
                provider: Arc::new(Legacy {
                    interface: a[3].clone(),
                    broker,
                }),
                events: Arc::new(report_event),
            },
            running,
        )
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
