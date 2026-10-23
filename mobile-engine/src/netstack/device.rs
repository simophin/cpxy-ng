//! Packet queues owned by the stack task. A full output queue backpressures smoltcp.
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use std::collections::VecDeque;

pub(super) const QUEUE_LEN: usize = 256;

pub(super) struct Device {
    pub rx: VecDeque<Vec<u8>>,
    pub tx: VecDeque<Vec<u8>>,
    mtu: u16,
}

impl Device {
    pub fn new(mtu: u16) -> Self {
        Self {
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            mtu,
        }
    }
}

pub(super) struct Rx(Vec<u8>);
pub(super) struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::Device for Device {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: Instant) -> Option<(Rx, Tx<'_>)> {
        if self.tx.len() >= QUEUE_LEN {
            return None;
        }
        self.rx
            .pop_front()
            .map(|packet| (Rx(packet), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _: Instant) -> Option<Tx<'_>> {
        (self.tx.len() < QUEUE_LEN).then(|| Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.mtu as usize;
        caps
    }
}

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        trace_tcp("rx", &self.0);
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        trace_tcp("tx", &packet);
        self.0.push_back(packet);
        result
    }
}

// Packet headers only, opt-in through RUST_LOG=mobile_engine::netstack::wire=trace.
// Avoid parsing and allocating unless this diagnostic target is enabled.
fn trace_tcp(direction: &str, packet: &[u8]) {
    if !tracing::enabled!(target: "mobile_engine::netstack::wire", tracing::Level::TRACE) {
        return;
    }
    if let Ok(packet) = etherparse::SlicedPacket::from_ip(packet)
        && let Some(etherparse::TransportSlice::Tcp(tcp)) = packet.transport
    {
        let options: Vec<_> = tcp.options_iterator().collect();
        tracing::trace!(target: "mobile_engine::netstack::wire",
            direction, src_port = tcp.source_port(), dst_port = tcp.destination_port(),
            seq = tcp.sequence_number(), ack = tcp.acknowledgment_number(),
            window = tcp.window_size(), payload = tcp.payload().len(),
            syn = tcp.syn(), fin = tcp.fin(), rst = tcp.rst(), ?options, "TCP packet");
    }
}
