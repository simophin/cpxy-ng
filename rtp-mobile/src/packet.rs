//! What happens to each packet between the TUN and the tunnel. Refused traffic gets an ICMP error
//! back, so apps fail fast instead of timing out.

use etherparse::{Icmpv4Type, Icmpv6Type, PacketBuilder, icmpv4, icmpv6};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ops::Range;

const PROTO_TCP: u8 = 6;
const PROTO_UDP: u8 = 17;
const PROTO_ICMPV6: u8 = 58;
const DNS_PORT: u16 = 53;
/// QUIC needs 1200-byte UDP payloads, which do not fit the tunnel's 1200-byte packets; refusing
/// it makes browsers fall back to TCP at once.
const QUIC_PORT: u16 = 443;
/// DNS over TLS (Android Private DNS): refused so Android falls back to the VPN's DNS server.
const DOT_PORT: u16 = 853;
/// Source ports of the DNS queries the engine sends through the tunnel: above Linux's ephemeral
/// range, so the device's own flows from the tunnel address do not use them.
pub const INJECT_PORTS: Range<u16> = 61000..65535;

const IPV6_HEADER_LEN: usize = 40;
/// The most of the offending packet an ICMPv6 error may carry (RFC 4443 §2.4).
const ICMPV6_MAX_QUOTE: usize = 1280 - IPV6_HEADER_LEN - 8;
const TTL: u8 = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// Send through the tunnel.
    Tunnel,
    /// A query to the virtual DNS server; the payload is at `payload`.
    Dns {
        src: Ipv4Addr,
        port: u16,
        payload: Range<usize>,
    },
    /// Write this ICMP error back to the TUN.
    Reply(Vec<u8>),
    Drop,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    /// Write to the TUN.
    Tun,
    /// The answer to a query the engine sent through the tunnel from `port`.
    DnsReply {
        port: u16,
        payload: Range<usize>,
    },
    Drop,
}

/// A packet the device sends into the TUN.
pub fn outgoing(packet: &[u8], own: Ipv4Addr, dns: Ipv4Addr) -> Outgoing {
    match packet.first().map(|b| b >> 4) {
        Some(4) => outgoing_v4(packet, own, dns),
        Some(6) => outgoing_v6(packet),
        _ => Outgoing::Drop,
    }
}

fn outgoing_v4(packet: &[u8], own: Ipv4Addr, dns: Ipv4Addr) -> Outgoing {
    let Some(v4) = Ipv4::parse(packet) else {
        return Outgoing::Drop;
    };
    if v4.src != own || v4.dst.is_multicast() || v4.dst.is_broadcast() {
        return Outgoing::Drop;
    }
    let refuse = || {
        // RFC 792: the error quotes the IP header and the first 8 bytes of its payload.
        let quote = &packet[..packet.len().min(v4.header_len + 8)];
        Outgoing::Reply(port_unreachable_v4(v4.dst, v4.src, quote))
    };
    let ports = v4.ports(packet);
    if v4.dst == dns {
        return match (v4.protocol, ports) {
            (PROTO_UDP, Some((port, DNS_PORT))) => match v4.udp_payload(packet) {
                Some(payload) => Outgoing::Dns {
                    src: v4.src,
                    port,
                    payload,
                },
                None => Outgoing::Drop,
            },
            (PROTO_TCP | PROTO_UDP, Some(_)) => refuse(),
            _ => Outgoing::Drop,
        };
    }
    match (v4.protocol, ports) {
        (PROTO_UDP, Some((_, QUIC_PORT))) | (PROTO_TCP, Some((_, DOT_PORT))) => refuse(),
        _ => Outgoing::Tunnel,
    }
}

fn outgoing_v6(packet: &[u8]) -> Outgoing {
    if packet.len() < IPV6_HEADER_LEN {
        return Outgoing::Drop;
    }
    let src = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[8..24]).unwrap());
    let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).unwrap());
    if packet[6] == PROTO_ICMPV6 || dst.is_multicast() || src.is_unspecified() {
        return Outgoing::Drop;
    }
    // The tunnel carries IPv4 only. "Administratively prohibited" is a hard error for TCP and UDP
    // sockets alike (unlike "no route", which UDP sockets ignore), so apps fall back to IPv4.
    let quote = &packet[..packet.len().min(ICMPV6_MAX_QUOTE)];
    let builder = PacketBuilder::ipv6(dst.octets(), src.octets(), TTL).icmpv6(
        Icmpv6Type::DestinationUnreachable(icmpv6::DestUnreachableCode::Prohibited),
    );
    let mut out = Vec::with_capacity(builder.size(quote.len()));
    builder.write(&mut out, quote).expect("writing to a Vec");
    Outgoing::Reply(out)
}

/// A packet that came out of the tunnel.
pub fn incoming(packet: &[u8], own: Ipv4Addr, resolvers: &[Ipv4Addr]) -> Incoming {
    let Some(v4) = Ipv4::parse(packet) else {
        return Incoming::Drop;
    };
    if v4.dst != own {
        return Incoming::Drop;
    }
    if v4.protocol == PROTO_UDP
        && resolvers.contains(&v4.src)
        && let Some((DNS_PORT, port)) = v4.ports(packet)
        && INJECT_PORTS.contains(&port)
        && let Some(payload) = v4.udp_payload(packet)
    {
        return Incoming::DnsReply { port, payload };
    }
    Incoming::Tun
}

/// An IPv4/UDP packet.
pub fn udp_v4(src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16, payload: &[u8]) -> Vec<u8> {
    let builder = PacketBuilder::ipv4(src.octets(), dst.octets(), TTL).udp(sport, dport);
    let mut out = Vec::with_capacity(builder.size(payload.len()));
    builder.write(&mut out, payload).expect("writing to a Vec");
    out
}

fn port_unreachable_v4(from: Ipv4Addr, to: Ipv4Addr, quote: &[u8]) -> Vec<u8> {
    let builder = PacketBuilder::ipv4(from.octets(), to.octets(), TTL).icmpv4(
        Icmpv4Type::DestinationUnreachable(icmpv4::DestUnreachableHeader::Port),
    );
    let mut out = Vec::with_capacity(builder.size(quote.len()));
    builder.write(&mut out, quote).expect("writing to a Vec");
    out
}

struct Ipv4 {
    header_len: usize,
    total_len: usize,
    protocol: u8,
    first_fragment: bool,
    src: Ipv4Addr,
    dst: Ipv4Addr,
}

impl Ipv4 {
    fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < 20 || p[0] >> 4 != 4 {
            return None;
        }
        let header_len = usize::from(p[0] & 0x0f) * 4;
        let total_len = usize::from(u16::from_be_bytes([p[2], p[3]]));
        if header_len < 20 || total_len < header_len || total_len > p.len() {
            return None;
        }
        Some(Self {
            header_len,
            total_len,
            protocol: p[9],
            first_fragment: u16::from_be_bytes([p[6], p[7]]) & 0x1fff == 0,
            src: Ipv4Addr::new(p[12], p[13], p[14], p[15]),
            dst: Ipv4Addr::new(p[16], p[17], p[18], p[19]),
        })
    }

    /// Source and destination ports of TCP or UDP, when the packet carries them.
    fn ports(&self, p: &[u8]) -> Option<(u16, u16)> {
        let h = self.header_len;
        (self.first_fragment && self.total_len >= h + 4).then(|| {
            (
                u16::from_be_bytes([p[h], p[h + 1]]),
                u16::from_be_bytes([p[h + 2], p[h + 3]]),
            )
        })
    }

    /// An unfragmented UDP datagram's payload.
    fn udp_payload(&self, p: &[u8]) -> Option<Range<usize>> {
        let more_fragments = p[6] & 0x20 != 0;
        let h = self.header_len;
        if self.protocol != PROTO_UDP || !self.first_fragment || more_fragments {
            return None;
        }
        let udp_len = usize::from(u16::from_be_bytes([*p.get(h + 4)?, *p.get(h + 5)?]));
        (udp_len >= 8 && h + udp_len <= self.total_len).then(|| h + 8..h + udp_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::{NetSlice, SlicedPacket, TransportSlice};

    const OWN: Ipv4Addr = Ipv4Addr::new(10, 88, 0, 2);
    const DNS: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 53);
    const REMOTE: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);
    const RESOLVER: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);

    fn tcp_v4(src: Ipv4Addr, dst: Ipv4Addr, dport: u16) -> Vec<u8> {
        let builder =
            PacketBuilder::ipv4(src.octets(), dst.octets(), 64).tcp(40000, dport, 1, 1000);
        let mut out = Vec::new();
        builder.write(&mut out, b"x").unwrap();
        out
    }

    fn icmp_kind(reply: &[u8]) -> String {
        let sliced = SlicedPacket::from_ip(reply).unwrap();
        match sliced.transport.unwrap() {
            TransportSlice::Icmpv4(i) => format!("{:?}", i.icmp_type()),
            TransportSlice::Icmpv6(i) => format!("{:?}", i.icmp_type()),
            _ => panic!("not ICMP"),
        }
    }

    #[test]
    fn ordinary_ipv4_goes_through_the_tunnel() {
        assert_eq!(
            outgoing(&tcp_v4(OWN, REMOTE, 443), OWN, DNS),
            Outgoing::Tunnel
        );
        assert_eq!(
            outgoing(&udp_v4(OWN, 5000, REMOTE, 3478, b"stun"), OWN, DNS),
            Outgoing::Tunnel
        );
    }

    #[test]
    fn foreign_sources_multicast_and_garbage_are_dropped() {
        let other = Ipv4Addr::new(10, 88, 0, 3);
        assert_eq!(
            outgoing(&tcp_v4(other, REMOTE, 80), OWN, DNS),
            Outgoing::Drop
        );
        let mdns = udp_v4(OWN, 5353, Ipv4Addr::new(224, 0, 0, 251), 5353, b"q");
        assert_eq!(outgoing(&mdns, OWN, DNS), Outgoing::Drop);
        assert_eq!(outgoing(&[0x45, 0, 0], OWN, DNS), Outgoing::Drop);
        let mut truncated = tcp_v4(OWN, REMOTE, 80);
        truncated.truncate(30);
        assert_eq!(outgoing(&truncated, OWN, DNS), Outgoing::Drop);
    }

    #[test]
    fn dns_to_the_virtual_server_is_answered_locally() {
        let packet = udp_v4(OWN, 41000, DNS, 53, b"query");
        let Outgoing::Dns { src, port, payload } = outgoing(&packet, OWN, DNS) else {
            panic!()
        };
        assert_eq!((src, port, &packet[payload]), (OWN, 41000, &b"query"[..]));
        let Outgoing::Reply(reply) = outgoing(&tcp_v4(OWN, DNS, 53), OWN, DNS) else {
            panic!()
        };
        assert!(icmp_kind(&reply).contains("Port"));
    }

    #[test]
    fn quic_dot_and_ipv6_are_refused() {
        for packet in [
            udp_v4(OWN, 5000, REMOTE, 443, b"quic"),
            tcp_v4(OWN, REMOTE, 853),
        ] {
            let Outgoing::Reply(reply) = outgoing(&packet, OWN, DNS) else {
                panic!()
            };
            let sliced = SlicedPacket::from_ip(&reply).unwrap();
            let Some(NetSlice::Ipv4(ip)) = sliced.net else {
                panic!()
            };
            assert_eq!(ip.header().source_addr(), REMOTE);
            assert_eq!(ip.header().destination_addr(), OWN);
            assert!(icmp_kind(&reply).contains("Port"));
        }
        let builder = PacketBuilder::ipv6(
            "fd00::1".parse::<Ipv6Addr>().unwrap().octets(),
            "2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap().octets(),
            64,
        )
        .tcp(40000, 443, 1, 1000);
        let mut v6 = Vec::new();
        builder.write(&mut v6, b"").unwrap();
        let Outgoing::Reply(reply) = outgoing(&v6, OWN, DNS) else {
            panic!()
        };
        assert!(icmp_kind(&reply).contains("Prohibited"));
    }

    #[test]
    fn tunnel_dns_replies_are_recognised() {
        let reply = udp_v4(RESOLVER, 53, OWN, 61005, b"answer");
        let Incoming::DnsReply { port, payload } = incoming(&reply, OWN, &[RESOLVER]) else {
            panic!()
        };
        assert_eq!((port, &reply[payload]), (61005, &b"answer"[..]));
        // An app's own query to the same resolver, from an ordinary port.
        let app = udp_v4(RESOLVER, 53, OWN, 40000, b"answer");
        assert_eq!(incoming(&app, OWN, &[RESOLVER]), Incoming::Tun);
        assert_eq!(
            incoming(&udp_v4(RESOLVER, 53, OWN, 61005, b"a"), OWN, &[]),
            Incoming::Tun
        );
        assert_eq!(
            incoming(
                &udp_v4(RESOLVER, 53, Ipv4Addr::new(10, 88, 0, 3), 1, b"a"),
                OWN,
                &[]
            ),
            Incoming::Drop
        );
    }
}
