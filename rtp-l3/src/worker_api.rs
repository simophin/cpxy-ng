/// Host operations supplied by the supervisor. A packet worker never chooses
/// firewall rules, routes or commands. Implementations must enforce the configured bind and allocation policy.
pub trait SocketProvider: std::marker::Send + Sync {
    /// Open a TCP control connection outside the protected tunnel.
    fn control_socket(&self, _peer: SocketAddr) -> io::Result<std::net::TcpStream> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "HTTP control socket protection is required"))
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

/// Unsupported legacy adapter shape, retained only for source compatibility.
#[derive(Clone)]
pub struct WorkerConfig {
    pub client: bool,
    pub psk: [u8; 32],
    pub inner: Ipv4Addr,
    pub prefix: u8,
    pub rotation_low: u16,
    pub rotation_high: u16,
    pub initial_rotation_port: Option<u16>,
    pub rotation_seconds: u64,
    pub keepalive_seconds: u64,
}
pub type Events = Arc<dyn Fn(serde_json::Value) + std::marker::Send + Sync>;
pub struct Resources {
    pub packet: Box<dyn PacketIo>,
    pub clock: Arc<dyn Clock>,
    pub socket: UdpSocket,
    pub provider: Arc<dyn SocketProvider>,
    pub events: Events,
}

/// Remove UDP peer filtering while preserving the source tuple.
#[cfg(target_os = "linux")]
pub fn unconnect(socket: &UdpSocket) -> io::Result<SocketAddr> {
    disconnect(socket)
}

/// Packet source/sink descriptor. Linux TUN is one implementation; tests and
/// platform adapters can supply packet-preserving descriptors of their own.
pub trait PacketIo: Read + Write + AsRawFd + std::marker::Send {}
impl<T: Read + Write + AsRawFd + std::marker::Send> PacketIo for T {}
pub trait Clock: std::marker::Send + Sync {
    fn elapsed(&self) -> Duration;
}
pub struct MonotonicClock(Instant);
impl Default for MonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for MonotonicClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}
