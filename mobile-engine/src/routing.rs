use client::outbound::{DirectOutbound, IPDivertOutbound, ProtocolOutbound, StatReportingOutbound};
use client::protocol_config::Config as ServerConfig;
use client::stats_server::OutboundEvent;
use cpxy_ng::geoip::find_country_code_v4;
use cpxy_ng::outbound::Outbound;
use geoip_data::GEOIP;
use std::borrow::Cow;
use std::net::Ipv4Addr;
use tokio::sync::broadcast;

/// TCP to private, loopback, link-local and CN addresses goes direct; everything else goes
/// through the cpxy server. Unlike `client::outbound::cn::cn_outbound`, there are no AI or
/// Tailscale branches, and flows always arrive as IPs, so nothing is resolved.
pub fn outbound(
    server: ServerConfig,
    proxy_routes: client::outbound::ProxyRoutes,
    events_tx: broadcast::Sender<OutboundEvent>,
) -> impl Outbound + Send + Sync + 'static {
    IPDivertOutbound {
        outbound_a: Some(StatReportingOutbound {
            name: Cow::Borrowed("direct"),
            inner: DirectOutbound::default(),
            events_tx: events_tx.clone(),
        }),
        outbound_b: StatReportingOutbound {
            name: Cow::Borrowed("proxy"),
            inner: ProtocolOutbound(server),
            events_tx,
        },
        should_use_a: move |ip: Option<Ipv4Addr>| {
            !proxy_routes.contains(ip) && ip.is_some_and(should_route_direct)
        },
    }
}

pub fn should_route_direct(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || matches!(find_country_code_v4(&ip, GEOIP), Ok(Some("CN")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixed_routes_override_direct_addresses() {
        use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("http://:key@{}", closed.local_addr().unwrap())
            .parse()
            .unwrap();
        drop(closed);
        let (tx, mut rx) = broadcast::channel(4);
        let outbound = outbound(
            server,
            client::outbound::ProxyRoutes::parse(&["223.5.5.5/32".into(), "10.0.0.0/8".into()])
                .unwrap(),
            tx,
        );
        for ip in ["223.5.5.5", "10.1.2.3"] {
            let _ = outbound
                .send(OutboundRequest {
                    host: OutboundHost::Resolved {
                        domain: ip.into(),
                        ip: Some(ip.parse().unwrap()),
                    },
                    port: 80,
                    tls: false,
                    initial_plaintext: vec![],
                })
                .await;
            assert!(
                matches!(rx.recv().await.unwrap(), OutboundEvent::Error { outbound, .. } if outbound == "proxy")
            );
        }
    }

    #[test]
    fn local_addresses_go_direct() {
        for ip in [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
        ] {
            assert!(should_route_direct(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn cn_addresses_go_direct() {
        // AliDNS, Baidu DNS, 114DNS. Not DNSPod's 119.29.29.29: DB-IP places that anycast
        // address in Singapore.
        for ip in ["223.5.5.5", "180.76.76.76", "114.114.114.114"] {
            assert!(should_route_direct(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn everything_else_is_proxied() {
        // Google, Cloudflare, example.com, Tailscale CGNAT (no Tailscale branch here)
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "100.100.100.100"] {
            assert!(!should_route_direct(ip.parse().unwrap()), "{ip}");
        }
    }
}
