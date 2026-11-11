//! CN split routing for the resolvers themselves, where nothing else routes their traffic: a
//! router's own sockets bypass its firewall's policy, so each resolver is classified by address.

use super::is_local_region_ip;
use super::spec::{Destination, Endpoint, ServerSpec, Via};
use crate::outbound::{ProxyRoutes, is_private};
use std::net::IpAddr;

/// Whether `ip` goes through the proxy: anything outside China and the private ranges, and
/// whatever `routes` forces. An unknown address is proxied.
pub fn should_proxy(routes: &ProxyRoutes, ip: Option<IpAddr>) -> bool {
    routes.contains(ip) || !ip.is_some_and(|ip| is_private(ip) || is_local_region_ip(ip))
}

/// `spec`, reached directly or through the proxy by its address. The proxy carries TCP only, so
/// a proxied UDP server is asked over TCP instead. A server already given with `?via=cpxy` stays
/// proxied.
pub async fn route_spec(spec: &ServerSpec, routes: &ProxyRoutes) -> anyhow::Result<ServerSpec> {
    let mut spec = spec.clone();
    if spec.via == Via::Cpxy {
        return Ok(spec);
    }
    let Destination::Addr(address) = spec.resolve().await? else {
        anyhow::bail!("DNS address must resolve before classification");
    };
    let proxy = should_proxy(routes, Some(address.ip()));
    spec.via = if proxy { Via::Cpxy } else { Via::Direct };
    match &mut spec.endpoint {
        Endpoint::Udp(_) if proxy => spec.endpoint = Endpoint::Tcp(address),
        Endpoint::Tls { ip, .. } | Endpoint::Https { ip, .. } => *ip = Some(address.ip()),
        _ => {}
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_split::upstream::{
        DnsUpstream, HickoryUpstream, OutboundDialer, tls_client_config,
    };
    use crate::handshaker::Handshaker;
    use crate::outbound::Socks5Outbound;
    use crate::socks_proxy_server::{ProxyRequest, SocksProxyHandshaker};
    use hickory_proto::op::{Message, MessageType, Query};
    use hickory_proto::rr::{Name, RecordType};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn forced_routes_override_direct_addresses() {
        let routes = ProxyRoutes::parse(&[
            "223.5.5.5/32".into(),
            "10.0.0.0/8".into(),
            "2400:3200::/32".into(),
        ])
        .unwrap();
        for ip in ["223.5.5.5", "10.1.2.3", "2400:3200::1"] {
            assert!(should_proxy(&routes, Some(ip.parse().unwrap())), "{ip}");
        }
        assert!(!should_proxy(&routes, Some("192.168.1.1".parse().unwrap())));
        assert!(should_proxy(&routes, None));
    }

    #[test]
    fn cn_and_private_go_direct_the_rest_is_proxied() {
        let none = ProxyRoutes(Vec::new());
        // AliDNS, Baidu DNS, 114DNS. Not DNSPod's 119.29.29.29: DB-IP places that anycast
        // address in Singapore.
        for ip in [
            "223.5.5.5",
            "180.76.76.76",
            "114.114.114.114",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
        ] {
            assert!(!should_proxy(&none, Some(ip.parse().unwrap())), "{ip}");
        }
        // Google, Cloudflare, example.com, Tailscale CGNAT
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "100.100.100.100"] {
            assert!(should_proxy(&none, Some(ip.parse().unwrap())), "{ip}");
        }
    }

    #[tokio::test]
    async fn resolver_transport_follows_destination_and_overrides_in_both_families() {
        let routes = ProxyRoutes::parse(&["223.5.5.5/32".into(), "2400:3200::/32".into()]).unwrap();
        for (input, proxy, tcp) in [
            ("114.114.114.114", false, false),
            ("8.8.8.8", true, true),
            ("223.5.5.5", true, true),
            ("[2400:3200::1]:53", true, true),
            ("[240e::1]:53", false, false),
            ("[2001:4860:4860::8888]:53", true, true),
            ("[fd00::1]:53", false, false),
            ("https://dns.google?ip=8.8.8.8", true, false),
        ] {
            let spec = route_spec(&input.parse().unwrap(), &routes).await.unwrap();
            assert_eq!(spec.via == Via::Cpxy, proxy, "{input}");
            assert_eq!(matches!(spec.endpoint, Endpoint::Tcp(_)), tcp, "{input}");
        }
    }

    #[tokio::test]
    async fn overseas_dns_reaches_socks5_as_tcp_with_original_address() {
        for destination in ["8.8.8.8:53", "[2001:4860:4860::8888]:53"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = listener.local_addr().unwrap();
            let expected: SocketAddr = destination.parse().unwrap();
            let fixture = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let (request, handshake) = SocksProxyHandshaker::accept(socket).await.unwrap();
                assert!(matches!(request, ProxyRequest::WithIP(addr) if addr == expected));
                let mut stream = handshake.respond_ok().await.unwrap();
                let length = stream.read_u16().await.unwrap();
                let mut bytes = vec![0; length as usize];
                stream.read_exact(&mut bytes).await.unwrap();
                let mut response = Message::from_vec(&bytes).unwrap();
                assert_eq!(response.queries()[0].name().to_ascii(), "example.com.");
                response.set_message_type(MessageType::Response);
                let bytes = response.to_vec().unwrap();
                stream.write_u16(bytes.len() as u16).await.unwrap();
                stream.write_all(&bytes).await.unwrap();
            });
            let spec = route_spec(&destination.parse().unwrap(), &ProxyRoutes::default())
                .await
                .unwrap();
            let upstream = HickoryUpstream::new(
                &spec,
                tls_client_config().unwrap(),
                Duration::from_secs(5),
                Some(Arc::new(OutboundDialer(Arc::new(Socks5Outbound(proxy))))),
            )
            .await
            .unwrap();
            let mut query = Message::new();
            query.add_query(Query::query(
                Name::from_ascii("example.com.").unwrap(),
                RecordType::A,
            ));
            let answer = upstream.query(query).await.unwrap();
            assert_eq!(answer.message_type(), MessageType::Response);
            fixture.await.unwrap();
        }
    }
}
