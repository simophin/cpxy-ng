/// Host operations supplied by the supervisor. A packet worker never chooses
/// firewall rules, routes or commands. Implementations must enforce the configured bind and allocation policy.
pub trait SocketProvider: std::marker::Send + Sync {
    /// Open a TCP control connection outside the protected tunnel.
    fn control_socket(&self, _peer: SocketAddr) -> io::Result<std::net::TcpStream> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "HTTP control socket protection is required",
        ))
    }

    /// Bind and admit a socket. Port zero requests an OS-assigned port; admit
    /// the actual bound port. Failure must not disturb any committed path.
    fn prepare_port(&self, address: SocketAddr) -> io::Result<UdpSocket>;
    /// Complete admission before exposing an allocation to a client.
    fn commit_port(&self, port: u16) -> io::Result<()>;
    /// Remove only this generation's lease. The worker still holds the socket.
    fn retire_port(&self, port: u16) -> io::Result<()>;
    fn reconcile_ports(&self) -> io::Result<()>;
    /// Every new client socket must bypass VPN capture, including repairs.
    fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket>;
    fn packet_ready(&self) -> io::Result<()> {
        Ok(())
    }
    /// Polled by a client about four times a second. True once after the host network changed
    /// (e.g. Wi-Fi to cellular): the client then replaces its path at once instead of waiting
    /// for PING timeouts.
    fn repair_requested(&self) -> bool {
        false
    }
}

pub type Events = Arc<dyn Fn(serde_json::Value) + std::marker::Send + Sync>;

/// Remove UDP peer filtering while preserving the source tuple.
#[cfg(target_os = "linux")]
pub fn unconnect(socket: &UdpSocket) -> io::Result<SocketAddr> {
    disconnect(socket)
}

/// Packet source/sink descriptor. Linux TUN is one implementation; tests and
/// platform adapters can supply packet-preserving descriptors of their own.
pub trait PacketIo: Read + Write + AsRawFd + std::marker::Send {}
impl<T: Read + Write + AsRawFd + std::marker::Send> PacketIo for T {}

/// Absorb WAN bursts on each transport socket, without changing host-wide UDP
/// defaults. Linux accounts roughly twice the requested bytes. Privileged hosts
/// can exceed rmem_max; mobile/unprivileged hosts retain the ordinary capped size.
pub fn buffer_transport(socket: &UdpSocket) -> io::Result<()> {
    let bytes: libc::c_int = 2 * 1024 * 1024;
    let set = |option| unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            (&bytes as *const libc::c_int).cast(),
            std::mem::size_of_val(&bytes) as _,
        )
    };
    if set(libc::SO_RCVBUF) < 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    {
        // Best effort: CAP_NET_ADMIN is available on the router/endpoint but is
        // deliberately not required by the portable client contract.
        set(libc::SO_RCVBUFFORCE);
    }
    Ok(())
}

/// Keep the bounded packet batches fair without sleeping through incoming data.
/// The timeout still advances control/health timers when every descriptor is idle.
pub fn wait_readable(
    packet: &dyn PacketIo,
    sockets: impl Iterator<Item = std::os::fd::RawFd>,
) -> io::Result<()> {
    wait(packet, false, sockets, 2).map(|_| ())
}

/// Wait up to `timeout_ms` for readable input, or for a writable packet device when
/// `write_pending`. False when nothing became ready.
pub fn wait(
    packet: &dyn PacketIo,
    write_pending: bool,
    sockets: impl Iterator<Item = std::os::fd::RawFd>,
    timeout_ms: i32,
) -> io::Result<bool> {
    let device = libc::pollfd {
        fd: packet.as_raw_fd(),
        events: if write_pending {
            libc::POLLIN | libc::POLLOUT
        } else {
            libc::POLLIN
        },
        revents: 0,
    };
    let mut descriptors: Vec<_> = std::iter::once(device)
        .chain(sockets.map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        }))
        .collect();
    let result =
        unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout_ms) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        return Ok(true);
    }
    Ok(result > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    struct Device(UnixDatagram);
    impl Read for Device {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.recv(buf)
        }
    }
    impl Write for Device {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.send(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl AsRawFd for Device {
        fn as_raw_fd(&self) -> std::os::fd::RawFd {
            self.0.as_raw_fd()
        }
    }

    #[test]
    fn wait_wakes_on_input_or_pending_writes_and_otherwise_times_out() {
        let (device, peer) = UnixDatagram::pair().unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let device = Device(device);
        let sockets = || std::iter::once(socket.as_raw_fd());
        let started = std::time::Instant::now();
        assert!(!wait(&device, false, sockets(), 20).unwrap());
        assert!(started.elapsed() >= std::time::Duration::from_millis(15));
        // A writable device only wakes the wait while writes are queued.
        assert!(wait(&device, true, sockets(), 1000).unwrap());
        peer.send(b"packet").unwrap();
        assert!(wait(&device, false, sockets(), 1000).unwrap());
        let mut buf = [0; 16];
        assert_eq!(device.0.recv(&mut buf).unwrap(), 6);
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender
            .send_to(b"frame", socket.local_addr().unwrap())
            .unwrap();
        assert!(wait(&device, false, sockets(), 1000).unwrap());
    }
}
