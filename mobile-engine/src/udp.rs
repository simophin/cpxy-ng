//! UDP from the apps, as flows of datagrams between one app socket and one destination. The
//! engine reads the datagrams out of the packets and builds the replies' packets itself.

use crate::tun::{Traffic, Tun};
use etherparse::PacketBuilder;
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const PROTO_UDP: u8 = 17;
const UDP_HEADER_LEN: usize = 8;
const TTL: u8 = 64;
/// Datagrams waiting for a flow's task; more are dropped, as on a congested link.
const QUEUE_LEN: usize = 64;

type Key = (SocketAddr, SocketAddr);

/// The open flows, by app socket and destination.
pub struct Flows {
    flows: Arc<Mutex<HashMap<Key, Entry>>>,
    next_id: u64,
    tun: Tun,
    mtu: u16,
    traffic: Arc<Traffic>,
}

struct Entry {
    id: u64,
    datagrams: mpsc::Sender<Vec<u8>>,
}

impl Flows {
    pub fn new(tun: Tun, mtu: u16, traffic: Arc<Traffic>) -> Self {
        Self {
            flows: Default::default(),
            next_id: 0,
            tun,
            mtu,
            traffic,
        }
    }

    /// Hands the datagram in a UDP packet from an app to its flow: a new one if it starts one.
    pub fn dispatch(&mut self, packet: &[u8]) -> Option<Flow> {
        let (key, payload) = parse(packet)?;
        let mut flows = self.flows.lock().unwrap();
        if let Some(entry) = flows.get(&key) {
            match entry.datagrams.try_send(payload.to_vec()) {
                Err(mpsc::error::TrySendError::Closed(_)) => {}
                // Delivered, or dropped while the flow is busy.
                _ => return None,
            }
        }
        let (datagrams, receiver) = mpsc::channel(QUEUE_LEN);
        datagrams.try_send(payload.to_vec()).ok()?;
        self.next_id += 1;
        flows.insert(
            key,
            Entry {
                id: self.next_id,
                datagrams,
            },
        );
        Some(Flow {
            key,
            id: self.next_id,
            datagrams: receiver,
            flows: self.flows.clone(),
            tun: self.tun.clone(),
            mtu: self.mtu,
            traffic: self.traffic.clone(),
        })
    }
}

/// The source and destination of a UDP packet, and its datagram. None for anything else,
/// including fragments: UDP datagrams too large for the MTU are not relayed.
fn parse(packet: &[u8]) -> Option<(Key, &[u8])> {
    let (src, dst, offset) = match packet.first()? >> 4 {
        4 => {
            let header_len = usize::from(packet[0] & 0x0f) * 4;
            let fragment = u16::from_be_bytes([*packet.get(6)?, *packet.get(7)?]) & 0x3fff;
            if header_len < 20 || packet.get(9) != Some(&PROTO_UDP) || fragment != 0 {
                return None;
            }
            let src: [u8; 4] = packet.get(12..16)?.try_into().ok()?;
            let dst: [u8; 4] = packet.get(16..20)?.try_into().ok()?;
            (IpAddr::from(src), IpAddr::from(dst), header_len)
        }
        6 if packet.get(6) == Some(&PROTO_UDP) => {
            let src: [u8; 16] = packet.get(8..24)?.try_into().ok()?;
            let dst: [u8; 16] = packet.get(24..40)?.try_into().ok()?;
            (IpAddr::from(src), IpAddr::from(dst), 40)
        }
        _ => return None,
    };
    let header = packet.get(offset..offset + UDP_HEADER_LEN)?;
    let port = |at: usize| u16::from_be_bytes([header[at], header[at + 1]]);
    let len = usize::from(port(4));
    let payload = packet.get(offset + UDP_HEADER_LEN..offset + len.max(UDP_HEADER_LEN))?;
    Some((
        (SocketAddr::new(src, port(0)), SocketAddr::new(dst, port(2))),
        payload,
    ))
}

/// The datagrams between one app socket and one destination.
pub struct Flow {
    key: Key,
    id: u64,
    datagrams: mpsc::Receiver<Vec<u8>>,
    flows: Arc<Mutex<HashMap<Key, Entry>>>,
    tun: Tun,
    mtu: u16,
    traffic: Arc<Traffic>,
}

impl Flow {
    /// Where the app sends its datagrams.
    pub fn peer_addr(&self) -> SocketAddr {
        self.key.1
    }

    /// The app's next datagram.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.datagrams.recv().await
    }

    /// Sends a datagram to the app, from the destination. Fails with `InvalidInput` when it
    /// would not fit the MTU.
    pub async fn send(&self, datagram: &[u8]) -> io::Result<()> {
        let (app, dst) = self.key;
        let builder = match (dst.ip(), app.ip()) {
            (IpAddr::V4(from), IpAddr::V4(to)) => {
                PacketBuilder::ipv4(from.octets(), to.octets(), TTL)
            }
            (IpAddr::V6(from), IpAddr::V6(to)) => {
                PacketBuilder::ipv6(from.octets(), to.octets(), TTL)
            }
            _ => unreachable!("a flow's addresses have one IP version"),
        }
        .udp(dst.port(), app.port());
        if builder.size(datagram.len()) > usize::from(self.mtu) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut packet = Vec::with_capacity(builder.size(datagram.len()));
        builder
            .write(&mut packet, datagram)
            .expect("writing to a Vec cannot fail");
        self.tun.send(&packet).await?;
        self.traffic
            .received
            .fetch_add(packet.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for Flow {
    fn drop(&mut self) {
        let mut flows = self.flows.lock().unwrap();
        if flows
            .get(&self.key)
            .is_some_and(|entry| entry.id == self.id)
        {
            flows.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn udp(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        PacketBuilder::ipv4([10, 233, 0, 1], [1, 2, 3, 4], 64)
            .udp(5000, 53)
            .write(&mut out, payload)
            .unwrap();
        out
    }

    #[test]
    fn parses_datagrams_including_empty_ones() {
        for payload in [&b""[..], b"hello"] {
            let mut packet = udp(payload);
            // Ethernet-style padding past the UDP length is not part of the datagram.
            packet.extend_from_slice(&[0; 4]);
            let ((src, dst), datagram) = parse(&packet).unwrap();
            assert_eq!(src, "10.233.0.1:5000".parse::<SocketAddr>().unwrap());
            assert_eq!(dst, "1.2.3.4:53".parse::<SocketAddr>().unwrap());
            assert_eq!(datagram, payload);
        }
        let mut fragment = udp(b"x");
        fragment[6] = 0x20;
        assert!(parse(&fragment).is_none());
        let truncated = udp(b"hello");
        assert!(parse(&truncated[..30]).is_none());
    }
}
