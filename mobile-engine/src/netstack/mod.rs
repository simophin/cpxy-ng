//! Async TCP streams over smoltcp and datagram-preserving UDP flows.
//!
//! Only the driver polls the interface. Flow IO holds the mutex briefly and wakes it after
//! consuming or queuing bytes, including reads that open the advertised receive window.
mod device;
mod tcp;
#[cfg(test)]
mod tests;
mod udp;

use device::Device;
use etherparse::{NetSlice, SlicedPacket, TransportSlice};
use smoltcp::iface::{Config as InterfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp::{self as smol_tcp, State};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpCidr};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
pub(crate) use tcp::TcpFlow;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;
pub(crate) use udp::UdpFlow;

type Pair = (SocketAddr, SocketAddr);

pub(crate) struct Config {
    pub mtu: u16,
    pub tcp_buffer: usize,
    pub tcp_idle_timeout: Duration,
    pub udp_idle_timeout: Duration,
    pub min_rto: Duration,
}

pub(crate) enum Flow {
    Tcp(TcpFlow),
    Udp(UdpFlow),
}

pub(crate) struct Stack {
    flows: mpsc::Receiver<Flow>,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

struct Shared {
    inner: Mutex<Inner>,
    notify: Notify,
}

struct Inner {
    running: bool,
    config: Config,
    epoch: Instant,
    iface: Interface,
    sockets: SocketSet<'static>,
    device: Device,
    tuples: HashMap<Pair, SocketHandle>,
    tcp: HashMap<SocketHandle, tcp::Entry>,
    udp: HashMap<Pair, udp::Entry>,
}

impl Stack {
    // Generic solely to allow a packet-preserving in-memory device in tests. Production uses TunDevice.
    pub fn new<D>(config: Config, device: D) -> Self
    where
        D: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let epoch = Instant::now();
        let mut queues = Device::new(config.mtu);
        let mut iface_config = InterfaceConfig::new(HardwareAddress::Ip);
        // ISNs must vary between stack lifetimes. No secret or persistent identity is needed.
        iface_config.random_seed = rand::random();
        let mut iface = Interface::new(iface_config, &mut queues, SmolInstant::ZERO);
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new("10.255.255.1".parse().unwrap(), 32))
                .unwrap();
            addrs
                .push(IpCidr::new("fd00:a:e9:ffff::1".parse().unwrap(), 128))
                .unwrap();
        });
        iface
            .routes_mut()
            .add_default_ipv4_route("10.255.255.1".parse().unwrap())
            .unwrap();
        iface
            .routes_mut()
            .add_default_ipv6_route("fd00:a:e9:ffff::1".parse().unwrap())
            .unwrap();
        iface.set_any_ip(true);
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                running: true,
                config,
                epoch,
                iface,
                sockets: SocketSet::new(vec![]),
                device: queues,
                tuples: HashMap::new(),
                tcp: HashMap::new(),
                udp: HashMap::new(),
            }),
            notify: Notify::new(),
        });
        let (tx, flows) = mpsc::channel(128);
        let task = tokio::spawn(drive(device, shared.clone(), tx));
        Self {
            flows,
            shared,
            task,
        }
    }

    pub async fn accept(&mut self) -> io::Result<Flow> {
        let flow = self
            .flows
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "IP stack stopped"))?;
        self.shared.notify.notify_one();
        Ok(flow)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.shared.stop();
        self.task.abort();
    }
}

impl Shared {
    fn stop(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.running = false;
        for entry in inner.tcp.values() {
            entry.control.fail();
        }
        for entry in inner.udp.values() {
            entry.identity.wake();
        }
        inner.udp.clear();
        self.notify.notify_one();
    }
}

fn reset() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionReset, "IP flow closed")
}

impl Inner {
    fn now(&self) -> SmolInstant {
        SmolInstant::from_micros(self.epoch.elapsed().as_micros().min(i64::MAX as u128) as i64)
    }

    fn ingest(&mut self, bytes: &[u8], shared: &Arc<Shared>, flows: &mpsc::Sender<Flow>) {
        let Ok(packet) = SlicedPacket::from_ip(bytes) else {
            return;
        };
        let (src, dst): (std::net::IpAddr, std::net::IpAddr) = match &packet.net {
            Some(NetSlice::Ipv4(ip)) if !ip.payload().fragmented => (
                ip.header().source_addr().into(),
                ip.header().destination_addr().into(),
            ),
            Some(NetSlice::Ipv6(ip)) if !ip.payload().fragmented => (
                ip.header().source_addr().into(),
                ip.header().destination_addr().into(),
            ),
            _ => return,
        };
        match packet.transport {
            Some(TransportSlice::Tcp(tcp)) => {
                if src.is_unspecified()
                    || dst.is_unspecified()
                    || tcp.source_port() == 0
                    || tcp.destination_port() == 0
                {
                    return;
                }
                let pair = (
                    SocketAddr::new(src, tcp.source_port()),
                    SocketAddr::new(dst, tcp.destination_port()),
                );
                if tcp.syn() && !tcp.ack() && !self.tuples.contains_key(&pair) {
                    let mut socket = smol_tcp::Socket::new(
                        smol_tcp::SocketBuffer::new(vec![0; self.config.tcp_buffer]),
                        smol_tcp::SocketBuffer::new(vec![0; self.config.tcp_buffer]),
                    );
                    socket.set_congestion_control(smol_tcp::CongestionControl::Cubic);
                    socket.set_nagle_enabled(false);
                    socket.set_keep_alive(None);
                    socket.set_min_rto(smoltcp::time::Duration::from_millis(
                        self.config.min_rto.as_millis() as u64,
                    ));
                    socket.listen(pair.1).expect("TCP destination endpoint");
                    let handle = self.sockets.add(socket);
                    self.tuples.insert(pair, handle);
                    self.tcp.insert(handle, tcp::Entry::new(pair));
                }
                if !tcp.payload().is_empty() {
                    if let Some(entry) = self.tuples.get(&pair).and_then(|h| self.tcp.get_mut(h)) {
                        entry.activity = Instant::now();
                    }
                }
                if self.device.rx.len() < device::QUEUE_LEN {
                    self.device.rx.push_back(bytes.to_vec());
                }
            }
            Some(TransportSlice::Udp(udp)) => {
                // Unlike TCP, UDP never enters smoltcp, so verify its checksum here too.
                let header = udp.to_header();
                let checksum = match (src, dst) {
                    (std::net::IpAddr::V4(src), std::net::IpAddr::V4(dst)) => {
                        if header.checksum == 0 {
                            Some(0)
                        } else {
                            header
                                .calc_checksum_ipv4_raw(src.octets(), dst.octets(), udp.payload())
                                .ok()
                        }
                    }
                    (std::net::IpAddr::V6(src), std::net::IpAddr::V6(dst)) => header
                        .calc_checksum_ipv6_raw(src.octets(), dst.octets(), udp.payload())
                        .ok(),
                    _ => None,
                };
                if checksum != Some(header.checksum) {
                    return;
                }
                let pair = (
                    SocketAddr::new(src, udp.source_port()),
                    SocketAddr::new(dst, udp.destination_port()),
                );
                self.ingest_udp(pair, udp.payload(), shared, flows);
            }
            _ => {}
        }
    }

    fn poll(&mut self, shared: &Arc<Shared>, flows: &mpsc::Sender<Flow>) -> Duration {
        let now = Instant::now();
        for (handle, entry) in &mut self.tcp {
            if now.duration_since(entry.activity) >= self.config.tcp_idle_timeout {
                self.sockets.get_mut::<smol_tcp::Socket>(*handle).abort();
                entry.control.fail();
            }
        }
        self.udp.retain(|_, entry| {
            let keep = !entry.tx.is_closed()
                && now.duration_since(entry.activity) < self.config.udp_idle_timeout;
            if !keep {
                entry.identity.wake();
            }
            keep
        });
        let timestamp = self.now();
        self.iface
            .poll(timestamp, &mut self.device, &mut self.sockets);
        self.tcp.retain(|handle, entry| {
            let socket = self.sockets.get::<smol_tcp::Socket>(*handle);
            if !entry.accepted && socket.state() == State::Established {
                if let Ok(permit) = flows.try_reserve() {
                    entry.accepted = true;
                    permit.send(Flow::Tcp(TcpFlow::new(
                        shared.clone(),
                        *handle,
                        entry.pair.1,
                        entry.control.clone(),
                    )));
                }
            }
            if socket.state() == State::Closed && entry.accepted && !entry.shutdown {
                entry.control.fail();
            }
            if (socket.state() == State::Listen && self.device.rx.is_empty())
                || (socket.state() == State::Closed
                    && (!entry.accepted || entry.dropped || entry.control.failed()))
            {
                entry.control.fail();
                self.sockets.remove(*handle);
                self.tuples.remove(&entry.pair);
                false
            } else {
                true
            }
        });
        let protocol_delay = self
            .iface
            .poll_delay(timestamp, &self.sockets)
            .map(|d| Duration::from_micros(d.total_micros()))
            .unwrap_or(Duration::from_secs(60));
        let tcp_idle = self
            .tcp
            .values()
            .map(|e| (e.activity + self.config.tcp_idle_timeout).saturating_duration_since(now));
        let udp_idle = self
            .udp
            .values()
            .map(|e| (e.activity + self.config.udp_idle_timeout).saturating_duration_since(now));
        tcp_idle.chain(udp_idle).fold(protocol_delay, Duration::min)
    }
}

async fn drive<D>(device: D, shared: Arc<Shared>, flows: mpsc::Sender<Flow>)
where
    D: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    struct Stop(Arc<Shared>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.stop();
        }
    }
    let _stop = Stop(shared.clone());
    let mut bytes = vec![0; 65535];
    let (mut reader, mut writer) = tokio::io::split(device);
    let mut output = std::collections::VecDeque::<Vec<u8>>::new();
    loop {
        let delay = {
            let mut inner = shared.inner.lock().unwrap();
            let delay = inner.poll(&shared, &flows);
            if output.is_empty() {
                output = std::mem::take(&mut inner.device.tx);
                for entry in inner.udp.values() {
                    entry.identity.wake();
                }
            }
            delay
        };
        // Keep receiving even when the device's write queue is full. Both peers can be
        // writing at once, and their ACKs must still reach smoltcp under backpressure.
        tokio::select! {
            result = reader.read(&mut bytes) => {
                match result {
                    Ok(0) | Err(_) => return,
                    Ok(n) => shared.inner.lock().unwrap().ingest(&bytes[..n], &shared, &flows),
                }
            }
            result = writer.write(output.front().map(Vec::as_slice).unwrap_or(&[])), if !output.is_empty() => {
                match result {
                    Ok(n) if n == output.front().unwrap().len() => { output.pop_front(); }
                    result => {
                        tracing::debug!(?result, "IP stack device packet write failed");
                        return;
                    }
                }
            }
            _ = shared.notify.notified() => {}
            _ = tokio::time::sleep(delay) => {}
        }
    }
}
