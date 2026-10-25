/// Host operations supplied by the supervisor. A packet worker never chooses
/// firewall rules, routes or commands. Implementations must enforce a port pool.
pub trait SocketProvider: std::marker::Send + Sync {
    /// Bind and admit a socket. Failure must not disturb any committed path.
    fn prepare_port(&self, address: SocketAddr) -> io::Result<UdpSocket>;
    /// Complete admission before the worker changes epochs or sends STATE.
    fn commit_port(&self, port: u16) -> io::Result<()>;
    /// Remove only this generation's lease. The worker still holds the socket.
    fn retire_port(&self, port: u16) -> io::Result<()>;
    fn reconcile_ports(&self) -> io::Result<()>;
    /// Every new client socket must bypass VPN capture, including repairs.
    fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket>;
    fn packet_ready(&self) -> io::Result<()> {
        Ok(())
    }
}

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
