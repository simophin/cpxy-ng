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
