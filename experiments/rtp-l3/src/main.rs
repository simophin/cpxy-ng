#[cfg(target_os = "linux")]
mod linux {
    use rtp_l3_poc::*;
    use std::{
        env, fs,
        io::{self, Read, Write},
        net::{Ipv4Addr, SocketAddr, UdpSocket},
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
    fn report_event(value: serde_json::Value) {
        // Serialize before taking stderr: shell job notifications can share this
        // file during namespace shutdown. Emit each JSON line in one write.
        let line = format!("{value}\n");
        let _ = io::stderr().lock().write_all(line.as_bytes());
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
        kernel_rx_drops: u64,
        sessions: usize,
        rotations: u64,
        announces_sent: u64,
        announce_retries: u64,
        switches: u64,
        pings_sent: u64,
        pings_unanswered: u64,
        fallbacks: u64,
        control_rejected: u64,
        unknown_destination: u64,
        session_conflicts: u64,
        rotation_bind_failures: u64,
        control_tx: u64,
        control_rx: u64,
    }
    impl Counts {
        fn report(&self, elapsed: f64, final_report: bool) {
            // There is no replay detection: `rejected` only covers malformed,
            // non-IPv4-after-XOR or foreign-source datagrams.
            let j = serde_json::json!({"event":if final_report{"final"}else{"stats"},"protocol":PROTOCOL,"seconds":elapsed,"tx_packets":self.tx_packets,"rx_packets":self.rx_packets,"tx_plain_bytes":self.tx_plain,"rx_plain_bytes":self.rx_plain,"tx_udp_payload_bytes":self.tx_wire,"rx_udp_payload_bytes":self.rx_wire,"tun_write_drops":self.tun_drops,"socket_send_drops":self.socket_drops,"socket_receive_drops":self.kernel_rx_drops,"rejected":self.invalid,"sessions":self.sessions,"rotations":self.rotations,"announces_sent":self.announces_sent,"announce_retries":self.announce_retries,"switches":self.switches,"pings_sent":self.pings_sent,"pings_unanswered":self.pings_unanswered,"fallbacks":self.fallbacks,"control_rejected":self.control_rejected,"unknown_destination":self.unknown_destination,"session_conflicts":self.session_conflicts,"rotation_bind_failures":self.rotation_bind_failures,"control_tx_packets":self.control_tx,"control_rx_packets":self.control_rx});
            report_event(j);
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
    include!("runtime.rs");
}
#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux runtime required; protocol library tests are portable");
}
