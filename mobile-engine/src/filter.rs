//! Decides, before the IP stack sees it, what happens to each packet the device sends into the
//! tunnel. Refused traffic gets an ICMP error back so apps fail fast instead of timing out.

use etherparse::{Icmpv4Type, Icmpv6Type, PacketBuilder, icmpv4, icmpv6};
use std::net::{Ipv4Addr, Ipv6Addr};

const PROTO_TCP: u8 = 6;
const PROTO_UDP: u8 = 17;
const PROTO_ICMP: u8 = 1;
const PROTO_ICMPV6: u8 = 58;

/// QUIC: refused so browsers fall back to TCP, which goes through the proxy.
const QUIC_PORT: u16 = 443;
/// DNS over TLS (Android Private DNS): refused so Android falls back to plain DNS, which the
/// engine answers.
const DOT_PORT: u16 = 853;

/// The most of the offending packet an ICMPv6 error may carry (RFC 4443 §2.4: the error must
/// fit the minimum IPv6 MTU of 1280 bytes).
const ICMPV6_MAX_QUOTE: usize = 1280 - 40 - 8;
const TTL: u8 = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Hand the packet to the IP stack.
    Pass,
    /// Discard the packet silently.
    Drop,
    /// Discard the packet and write this ICMP error back into the tunnel.
    Reply(Vec<u8>),
}

pub fn classify(packet: &[u8]) -> Verdict {
    match packet.first().map(|b| b >> 4) {
        Some(4) => classify_v4(packet),
        Some(6) => classify_v6(packet),
        _ => Verdict::Drop,
    }
}

fn classify_v4(packet: &[u8]) -> Verdict {
    let header_len = usize::from(packet[0] & 0x0f) * 4;
    if header_len < 20 || packet.len() < header_len {
        // Malformed: let the IP stack discard it.
        return Verdict::Pass;
    }

    let protocol = packet[9];
    let src = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
    let dst = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    if dst.is_multicast() || dst.is_broadcast() || protocol == PROTO_ICMP {
        // Neither is relayed, and an ICMP error must never answer these.
        return Verdict::Drop;
    }

    let fragment_offset = u16::from_be_bytes([packet[6], packet[7]]) & 0x1fff;
    if fragment_offset != 0 || packet.len() < header_len + 4 {
        // No transport header to look at.
        return Verdict::Pass;
    }

    let dst_port = u16::from_be_bytes([packet[header_len + 2], packet[header_len + 3]]);
    match (protocol, dst_port) {
        (PROTO_UDP, QUIC_PORT) | (PROTO_TCP, DOT_PORT) => {
            // RFC 792: the error quotes the IP header and the first 8 bytes of its payload.
            let quote = &packet[..packet.len().min(header_len + 8)];
            Verdict::Reply(port_unreachable_v4(dst, src, quote))
        }
        _ => Verdict::Pass,
    }
}

fn classify_v6(packet: &[u8]) -> Verdict {
    if packet.len() < 40 {
        return Verdict::Drop;
    }

    let next_header = packet[6];
    let src = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[8..24]).unwrap());
    let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).unwrap());
    if next_header == PROTO_ICMPV6 || dst.is_multicast() || src.is_unspecified() {
        return Verdict::Drop;
    }

    // IPv6 is not supported: the GeoIP data only covers IPv4.
    let quote = &packet[..packet.len().min(ICMPV6_MAX_QUOTE)];
    Verdict::Reply(no_route_v6(dst, src, quote))
}

fn port_unreachable_v4(from: Ipv4Addr, to: Ipv4Addr, quote: &[u8]) -> Vec<u8> {
    let builder = PacketBuilder::ipv4(from.octets(), to.octets(), TTL).icmpv4(
        Icmpv4Type::DestinationUnreachable(icmpv4::DestUnreachableHeader::Port),
    );
    let mut out = Vec::with_capacity(builder.size(quote.len()));
    builder
        .write(&mut out, quote)
        .expect("writing to a Vec cannot fail");
    out
}

fn no_route_v6(from: Ipv6Addr, to: Ipv6Addr, quote: &[u8]) -> Vec<u8> {
    let builder = PacketBuilder::ipv6(from.octets(), to.octets(), TTL).icmpv6(
        Icmpv6Type::DestinationUnreachable(icmpv6::DestUnreachableCode::NoRoute),
    );
    let mut out = Vec::with_capacity(builder.size(quote.len()));
    builder
        .write(&mut out, quote)
        .expect("writing to a Vec cannot fail");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::{NetSlice, SlicedPacket, TransportSlice};

    const APP: Ipv4Addr = Ipv4Addr::new(10, 233, 0, 1);
    const REMOTE: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);

    fn udp_v4(dst: Ipv4Addr, port: u16) -> Vec<u8> {
        let builder = PacketBuilder::ipv4(APP.octets(), dst.octets(), 64).udp(40000, port);
        let mut out = Vec::new();
        builder.write(&mut out, b"hello").unwrap();
        out
    }

    fn tcp_syn_v4(dst: Ipv4Addr, port: u16) -> Vec<u8> {
        let builder = PacketBuilder::ipv4(APP.octets(), dst.octets(), 64)
            .tcp(40000, port, 1234, 65535)
            .syn();
        let mut out = Vec::new();
        builder.write(&mut out, &[]).unwrap();
        out
    }

    fn tcp_syn_v6(dst: Ipv6Addr) -> Vec<u8> {
        let src: Ipv6Addr = "fd00::1".parse().unwrap();
        let builder = PacketBuilder::ipv6(src.octets(), dst.octets(), 64)
            .tcp(40000, 443, 1234, 65535)
            .syn();
        let mut out = Vec::new();
        builder.write(&mut out, &[]).unwrap();
        out
    }

    fn reply(verdict: Verdict) -> Vec<u8> {
        match verdict {
            Verdict::Reply(packet) => packet,
            v => panic!("expected a reply, got {v:?}"),
        }
    }

    #[test]
    fn ordinary_traffic_passes() {
        assert_eq!(classify(&tcp_syn_v4(REMOTE, 443)), Verdict::Pass);
        assert_eq!(classify(&tcp_syn_v4(REMOTE, 80)), Verdict::Pass);
        assert_eq!(classify(&udp_v4(REMOTE, 9999)), Verdict::Pass);
        assert_eq!(classify(&udp_v4(REMOTE, 53)), Verdict::Pass);
    }

    #[test]
    fn quic_gets_port_unreachable() {
        let request = udp_v4(REMOTE, 443);
        let packet = reply(classify(&request));

        let sliced = SlicedPacket::from_ip(&packet).unwrap();
        let Some(NetSlice::Ipv4(ip)) = sliced.net else {
            panic!("not IPv4")
        };
        assert_eq!(ip.header().source_addr(), REMOTE);
        assert_eq!(ip.header().destination_addr(), APP);
        let Some(TransportSlice::Icmpv4(icmp)) = sliced.transport else {
            panic!("not ICMP")
        };
        assert_eq!(
            icmp.icmp_type(),
            Icmpv4Type::DestinationUnreachable(icmpv4::DestUnreachableHeader::Port)
        );
        // The quote is the original IP header plus the 8-byte UDP header.
        assert_eq!(icmp.payload(), &request[..28]);
    }

    #[test]
    fn dns_over_tls_gets_port_unreachable() {
        reply(classify(&tcp_syn_v4(Ipv4Addr::new(8, 8, 8, 8), 853)));
        // Only TCP: UDP 853 (DNS over QUIC) is relayed like any other UDP
        assert_eq!(
            classify(&udp_v4(Ipv4Addr::new(8, 8, 8, 8), 853)),
            Verdict::Pass
        );
    }

    #[test]
    fn ipv6_gets_no_route() {
        let dst: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let packet = reply(classify(&tcp_syn_v6(dst)));

        let sliced = SlicedPacket::from_ip(&packet).unwrap();
        let Some(NetSlice::Ipv6(ip)) = sliced.net else {
            panic!("not IPv6")
        };
        assert_eq!(ip.header().source_addr(), dst);
        let Some(TransportSlice::Icmpv6(icmp)) = sliced.transport else {
            panic!("not ICMPv6")
        };
        assert_eq!(
            icmp.icmp_type(),
            Icmpv6Type::DestinationUnreachable(icmpv6::DestUnreachableCode::NoRoute)
        );
    }

    #[test]
    fn icmp_and_multicast_are_dropped_without_reply() {
        let mut ping = Vec::new();
        PacketBuilder::ipv4(APP.octets(), REMOTE.octets(), 64)
            .icmpv4_echo_request(1, 1)
            .write(&mut ping, b"ping")
            .unwrap();
        assert_eq!(classify(&ping), Verdict::Drop);

        let mdns = udp_v4(Ipv4Addr::new(224, 0, 0, 251), 5353);
        assert_eq!(classify(&mdns), Verdict::Drop);
        assert_eq!(classify(&udp_v4(Ipv4Addr::BROADCAST, 443)), Verdict::Drop);

        let mut neighbour_solicitation = Vec::new();
        PacketBuilder::ipv6(
            "fe80::1".parse::<Ipv6Addr>().unwrap().octets(),
            "ff02::1:ff00:1".parse::<Ipv6Addr>().unwrap().octets(),
            255,
        )
        .icmpv6_echo_request(1, 1)
        .write(&mut neighbour_solicitation, &[])
        .unwrap();
        assert_eq!(classify(&neighbour_solicitation), Verdict::Drop);
    }

    #[test]
    fn garbage_is_dropped_or_passed_without_panicking() {
        assert_eq!(classify(&[]), Verdict::Drop);
        assert_eq!(classify(&[0x00, 0x01]), Verdict::Drop);
        assert_eq!(classify(&[0x45, 0x00]), Verdict::Pass);
        assert_eq!(classify(&[0x60; 20]), Verdict::Drop);
        // A non-first fragment of a UDP 443 datagram has no port to look at
        let mut fragment = udp_v4(REMOTE, 443);
        fragment[6] = 0x00;
        fragment[7] = 0x10;
        assert_eq!(classify(&fragment), Verdict::Pass);
    }
}
