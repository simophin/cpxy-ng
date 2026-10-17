use crate::outbound::{
    DirectOutbound, IPDivertOutbound, ProtocolOutbound, ProxyRoutes, ResolvingIPOutbound,
    SiteDivertOutbound, StatReportingOutbound, is_private,
};
use crate::protocol_config::Config;
use crate::stats_server::OutboundEvent;
use cpxy_ng::geoip::{contains_v4, contains_v6};
use cpxy_ng::outbound::Outbound;
use geoip_data::{CN, CN6};
use hickory_resolver::Resolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::xfer::Protocol;
use ipnet::{Ipv4Net, Ipv6Net};
use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::broadcast;

pub fn cn_outbound(
    dns_servers: Vec<SocketAddr>,
    main_server: Config,
    ai_server: Option<Config>,
    tailscale_server: Option<Config>,
    proxy_routes: ProxyRoutes,
    events_tx: broadcast::Sender<OutboundEvent>,
) -> impl Outbound {
    let fixed_outbound = StatReportingOutbound {
        name: Cow::Borrowed("global"),
        inner: ProtocolOutbound(main_server.clone()),
        events_tx: events_tx.clone(),
    };
    let global_outbound = StatReportingOutbound {
        name: Cow::Borrowed("global"),
        inner: ProtocolOutbound(main_server),
        events_tx: events_tx.clone(),
    };

    let ai_outbound = ai_server.map(|c| StatReportingOutbound {
        name: Cow::Borrowed("ai"),
        inner: ProtocolOutbound(c),
        events_tx: events_tx.clone(),
    });
    let tailscale_outbound = tailscale_server.map(|c| StatReportingOutbound {
        name: Cow::Borrowed("tailscale"),
        inner: ProtocolOutbound(c),
        events_tx: events_tx.clone(),
    });

    let direct_outbound = StatReportingOutbound {
        name: Cow::Borrowed("direct"),
        inner: DirectOutbound::default(),
        events_tx,
    };

    let mut resolver_config = ResolverConfig::new();
    for dns_server in dns_servers {
        resolver_config.add_name_server(NameServerConfig::new(dns_server, Protocol::Udp));
    }

    ResolvingIPOutbound {
        inner: IPDivertOutbound {
            outbound_a: Some(fixed_outbound),
            should_use_a: move |ip| proxy_routes.contains(ip),
            outbound_b: IPDivertOutbound {
                outbound_a: tailscale_outbound,
                outbound_b: SiteDivertOutbound {
                    outbound_a: ai_outbound,
                    outbound_b: IPDivertOutbound {
                        outbound_a: Some(direct_outbound),
                        outbound_b: global_outbound,
                        should_use_a: ip_should_route_direct,
                    },
                    should_use_a: site_should_route_ai,
                },
                should_use_a: ip_should_route_tailscale,
            },
        },
        resolver: Arc::new(
            Resolver::builder_with_config(resolver_config, TokioConnectionProvider::default())
                .build(),
        ),
    }
}

const TAILSCALE_NETWORK: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(100, 0, 0, 0), 8);
const TAILSCALE_NETWORK_V6: Ipv6Net =
    Ipv6Net::new_assert(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0), 48);

fn ip_should_route_direct(ip: Option<IpAddr>) -> bool {
    match ip {
        Some(ip) => {
            is_private(ip)
                || ip_should_route_tailscale(Some(ip))
                || match ip {
                    IpAddr::V4(ip) => contains_v4(&ip, CN),
                    IpAddr::V6(ip) => contains_v6(&ip, CN6),
                }
        }
        _ => true,
    }
}

fn site_should_route_ai(domain: &str) -> bool {
    domain.contains("openai.com") || domain.contains("gemini") || domain.contains("anthropic")
}

fn ip_should_route_tailscale(ip: Option<IpAddr>) -> bool {
    match ip {
        Some(IpAddr::V4(ip)) => TAILSCALE_NETWORK.contains(&ip),
        Some(IpAddr::V6(ip)) => TAILSCALE_NETWORK_V6.contains(&ip),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn fixed_routes_override_cn_ai_and_tailscale_after_resolution() {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server: Config = format!("http://:key@{}", closed.local_addr().unwrap())
            .parse()
            .unwrap();
        drop(closed);
        let (tx, mut rx) = broadcast::channel(16);
        let outbound = cn_outbound(
            vec![],
            server.clone(),
            Some(server.clone()),
            Some(server),
            ProxyRoutes::parse(&[
                "223.5.5.5/32".into(),
                "100.0.0.0/8".into(),
                "127.0.0.0/8".into(),
            ])
            .unwrap(),
            tx,
        );
        for host in [
            OutboundHost::Resolved {
                domain: "cn.example".into(),
                ip: Some("223.5.5.5".parse().unwrap()),
            },
            OutboundHost::Resolved {
                domain: "openai.com".into(),
                ip: Some("223.5.5.5".parse().unwrap()),
            },
            OutboundHost::Resolved {
                domain: "tail.example".into(),
                ip: Some("100.100.100.100".parse().unwrap()),
            },
            // ResolvingIPOutbound must parse this literal before testing the route.
            OutboundHost::Domain("127.0.0.1".into()),
        ] {
            let _ = outbound
                .send(OutboundRequest {
                    host,
                    port: 80,
                    tls: false,
                    initial_plaintext: vec![],
                })
                .await;
            let event = rx.recv().await.unwrap();
            assert!(matches!(event, OutboundEvent::Error { outbound, .. } if outbound == "global"));
        }
    }

    #[tokio::test]
    async fn unmatched_local_destination_stays_direct() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let outbound = cn_outbound(
            vec![],
            "http://:key@127.0.0.1:1".parse().unwrap(),
            None,
            None,
            ProxyRoutes::parse(&["223.5.5.5/32".into()]).unwrap(),
            tx,
        );
        let stream = outbound
            .send(OutboundRequest {
                host: OutboundHost::Domain("127.0.0.1".into()),
                port: destination.local_addr().unwrap().port(),
                tls: false,
                initial_plaintext: vec![],
            })
            .await
            .unwrap();
        assert!(
            matches!(rx.recv().await.unwrap(), OutboundEvent::Connected { outbound, .. } if outbound == "direct")
        );
        drop(stream);
    }
}
