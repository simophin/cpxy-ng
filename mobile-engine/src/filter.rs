//! Decides, before the IP stack sees it, what happens to each packet the device sends into the
//! tunnel. Refused traffic gets an ICMP error back so apps fail fast instead of timing out.

use crate::routing::{Route, route};
use client::ipv6::Ipv6Mode;
use client::outbound::ProxyRoutes;
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

const IPV6_HEADER_LEN: usize = 40;
const TCP_SYN: u8 = 0x02;
const TCP_ACK: u8 = 0x10;
/// The most of the offending packet an ICMPv6 error may carry (RFC 4443 §2.4: the error must
/// fit the minimum IPv6 MTU of 1280 bytes).
const ICMPV6_MAX_QUOTE: usize = 1280 - IPV6_HEADER_LEN - 8;
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

pub struct Filter {
    ipv6: Ipv6Mode,
    /// Where TCP goes.
    proxy_routes: ProxyRoutes,
    /// Whether the engine's own sockets reach an address.
    has_route: fn(Ipv6Addr) -> bool,
}

impl Filter {
    pub fn new(ipv6: Ipv6Mode, proxy_routes: ProxyRoutes) -> Self {
        Self {
            ipv6,
            proxy_routes,
            has_route,
        }
    }

    pub fn classify(&self, packet: &[u8]) -> Verdict {
        match packet.first().map(|b| b >> 4) {
            Some(4) => classify_v4(packet),
            Some(6) => self.classify_v6(packet),
            _ => Verdict::Drop,
        }
    }

    fn classify_v6(&self, packet: &[u8]) -> Verdict {
        if packet.len() < IPV6_HEADER_LEN {
            return Verdict::Drop;
        }

        let next_header = packet[6];
        let src = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[8..24]).unwrap());
        let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).unwrap());
        if next_header == PROTO_ICMPV6 || dst.is_multicast() || src.is_unspecified() {
            return Verdict::Drop;
        }

        // Refused before the IP stack accepts a TCP connection: once accepted, a connection that
        // cannot be made fails after the app considers it open, which Happy Eyeballs does not
        // recover from. UDP is relayed direct, so it never needs the cpxy server.
        let direct = || route(&self.proxy_routes, Some(dst.into())) == Route::Direct;
        let allowed = match self.ipv6 {
            Ipv6Mode::Off => false,
            Ipv6Mode::Direct => next_header == PROTO_UDP || direct(),
            Ipv6Mode::Full => true,
        } && !(is_tcp_syn_v6(packet) && direct() && !(self.has_route)(dst));
        if !allowed {
            // "Administratively prohibited" is a hard error for both TCP and UDP sockets (unlike
            // "no route", which UDP sockets ignore unless they ask for ICMP errors), so apps fall
            // back to IPv4 at once.
            let quote = &packet[..packet.len().min(ICMPV6_MAX_QUOTE)];
            return Verdict::Reply(icmp_error_v6(
                dst,
                src,
                icmpv6::DestUnreachableCode::Prohibited,
                quote,
            ));
        }

        // Extension headers would hide the port; the IP stack drops such packets anyway.
        if packet.len() < IPV6_HEADER_LEN + 4 {
            return Verdict::Pass;
        }
        let dst_port = u16::from_be_bytes([packet[42], packet[43]]);
        match (next_header, dst_port) {
            (PROTO_UDP, QUIC_PORT) | (PROTO_TCP, DOT_PORT) => {
                let quote = &packet[..packet.len().min(ICMPV6_MAX_QUOTE)];
                Verdict::Reply(icmp_error_v6(
                    dst,
                    src,
                    icmpv6::DestUnreachableCode::Port,
                    quote,
                ))
            }
            _ => Verdict::Pass,
        }
    }
}

/// The first packet of a TCP connection, without extension headers.
fn is_tcp_syn_v6(packet: &[u8]) -> bool {
    packet[6] == PROTO_TCP
        && packet
            .get(IPV6_HEADER_LEN + 13)
            .is_some_and(|flags| flags & (TCP_SYN | TCP_ACK) == TCP_SYN)
}

/// Whether this host routes to `dst`, as when the network has no IPv6. A connected UDP socket
/// looks the route up without sending anything.
fn has_route(dst: Ipv6Addr) -> bool {
    std::net::UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0))
        .and_then(|socket| socket.connect((dst, 9)))
        .is_ok()
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

fn icmp_error_v6(
    from: Ipv6Addr,
    to: Ipv6Addr,
    code: icmpv6::DestUnreachableCode,
    quote: &[u8],
) -> Vec<u8> {
    let builder = PacketBuilder::ipv6(from.octets(), to.octets(), TTL)
        .icmpv6(Icmpv6Type::DestinationUnreachable(code));
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
    const APP_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
    /// Not in the local region.
    const REMOTE_V6: Ipv6Addr = Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888);
    /// AliDNS, in the local region.
    const LOCAL_V6: Ipv6Addr = Ipv6Addr::new(0x2400, 0x3200, 0, 0, 0, 0, 0, 1);

    /// A filter on a host that routes everywhere, whatever this one's network.
    fn routed(mode: Ipv6Mode, proxy_routes: ProxyRoutes) -> Filter {
        Filter {
            has_route: |_| true,
            ..Filter::new(mode, proxy_routes)
        }
    }

    fn classify(packet: &[u8]) -> Verdict {
        routed(Ipv6Mode::Full, ProxyRoutes::default()).classify(packet)
    }

    fn classify_with(mode: Ipv6Mode, packet: &[u8]) -> Verdict {
        routed(mode, ProxyRoutes::default()).classify(packet)
    }

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

    fn udp_v6(dst: Ipv6Addr, port: u16) -> Vec<u8> {
        let builder = PacketBuilder::ipv6(APP_V6.octets(), dst.octets(), 64).udp(40000, port);
        let mut out = Vec::new();
        builder.write(&mut out, b"hello").unwrap();
        out
    }

    fn tcp_syn_v6(dst: Ipv6Addr, port: u16) -> Vec<u8> {
        let builder = PacketBuilder::ipv6(APP_V6.octets(), dst.octets(), 64)
            .tcp(40000, port, 1234, 65535)
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

    /// The ICMPv6 error in `packet`, checking that it answers `request`.
    fn icmpv6_error(packet: &[u8], request: &[u8]) -> Icmpv6Type {
        let sliced = SlicedPacket::from_ip(packet).unwrap();
        let Some(NetSlice::Ipv6(ip)) = sliced.net else {
            panic!("not IPv6")
        };
        assert_eq!(ip.header().source_addr().octets(), request[24..40]);
        assert_eq!(ip.header().destination_addr().octets(), request[8..24]);
        let Some(TransportSlice::Icmpv6(icmp)) = sliced.transport else {
            panic!("not ICMPv6")
        };
        assert_eq!(icmp.payload(), request);
        icmp.icmp_type()
    }

    #[test]
    fn ordinary_traffic_passes() {
        assert_eq!(classify(&tcp_syn_v4(REMOTE, 443)), Verdict::Pass);
        assert_eq!(classify(&tcp_syn_v4(REMOTE, 80)), Verdict::Pass);
        assert_eq!(classify(&udp_v4(REMOTE, 9999)), Verdict::Pass);
        assert_eq!(classify(&udp_v4(REMOTE, 53)), Verdict::Pass);
        assert_eq!(classify(&tcp_syn_v6(REMOTE_V6, 443)), Verdict::Pass);
        assert_eq!(classify(&udp_v6(REMOTE_V6, 53)), Verdict::Pass);
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
    fn quic_and_dns_over_tls_over_ipv6_get_port_unreachable() {
        let port_unreachable =
            Icmpv6Type::DestinationUnreachable(icmpv6::DestUnreachableCode::Port);
        for request in [udp_v6(REMOTE_V6, 443), tcp_syn_v6(REMOTE_V6, 853)] {
            let packet = reply(classify(&request));
            assert_eq!(icmpv6_error(&packet, &request), port_unreachable);
        }
        assert_eq!(classify(&udp_v6(REMOTE_V6, 853)), Verdict::Pass);
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
    fn ipv6_gets_prohibited_when_off() {
        let prohibited =
            Icmpv6Type::DestinationUnreachable(icmpv6::DestUnreachableCode::Prohibited);
        for request in [tcp_syn_v6(LOCAL_V6, 443), udp_v6(REMOTE_V6, 9999)] {
            let packet = reply(classify_with(Ipv6Mode::Off, &request));
            assert_eq!(icmpv6_error(&packet, &request), prohibited);
        }
    }

    #[test]
    fn direct_ipv6_refuses_tcp_that_would_go_through_the_proxy() {
        let prohibited =
            Icmpv6Type::DestinationUnreachable(icmpv6::DestUnreachableCode::Prohibited);
        let request = tcp_syn_v6(REMOTE_V6, 443);
        let packet = reply(classify_with(Ipv6Mode::Direct, &request));
        assert_eq!(icmpv6_error(&packet, &request), prohibited);

        for request in [
            tcp_syn_v6(LOCAL_V6, 443),
            tcp_syn_v6(APP_V6, 22),
            udp_v6(REMOTE_V6, 9999),
        ] {
            assert_eq!(classify_with(Ipv6Mode::Direct, &request), Verdict::Pass);
        }

        // Fixed proxy routes go through the proxy even when the address is local.
        let routes = ProxyRoutes::parse(&["2400:3200::/32".into()]).unwrap();
        let filter = routed(Ipv6Mode::Direct, routes);
        reply(filter.classify(&tcp_syn_v6(LOCAL_V6, 443)));
    }

    #[test]
    fn direct_ipv6_tcp_is_refused_without_a_route() {
        for mode in [Ipv6Mode::Direct, Ipv6Mode::Full] {
            let filter = Filter {
                has_route: |_| false,
                ..Filter::new(mode, ProxyRoutes::default())
            };
            reply(filter.classify(&tcp_syn_v6(LOCAL_V6, 443)));
            // Only new connections are checked; UDP is relayed regardless.
            let mut ack = tcp_syn_v6(LOCAL_V6, 443);
            ack[IPV6_HEADER_LEN + 13] = TCP_ACK;
            assert_eq!(filter.classify(&ack), Verdict::Pass);
            assert_eq!(filter.classify(&udp_v6(LOCAL_V6, 9999)), Verdict::Pass);
        }
        // Proxied TCP needs only the cpxy server's IPv6.
        let filter = Filter {
            has_route: |_| false,
            ..Filter::new(Ipv6Mode::Full, ProxyRoutes::default())
        };
        assert_eq!(filter.classify(&tcp_syn_v6(REMOTE_V6, 443)), Verdict::Pass);
        assert!(has_route(Ipv6Addr::LOCALHOST));
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
        for mode in [Ipv6Mode::Off, Ipv6Mode::Direct, Ipv6Mode::Full] {
            assert_eq!(classify_with(mode, &neighbour_solicitation), Verdict::Drop);
            let mdns = udp_v6("ff02::fb".parse().unwrap(), 5353);
            assert_eq!(classify_with(mode, &mdns), Verdict::Drop);
        }
    }

    #[test]
    fn garbage_is_dropped_or_passed_without_panicking() {
        assert_eq!(classify(&[]), Verdict::Drop);
        assert_eq!(classify(&[0x00, 0x01]), Verdict::Drop);
        assert_eq!(classify(&[0x45, 0x00]), Verdict::Pass);
        assert_eq!(classify(&[0x60; 20]), Verdict::Drop);
        assert_eq!(classify(&tcp_syn_v6(REMOTE_V6, 853)[..42]), Verdict::Pass);
        // A non-first fragment of a UDP 443 datagram has no port to look at
        let mut fragment = udp_v4(REMOTE, 443);
        fragment[6] = 0x00;
        fragment[7] = 0x10;
        assert_eq!(classify(&fragment), Verdict::Pass);
    }
}
