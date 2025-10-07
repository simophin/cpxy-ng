use crate::outbound::{
    DirectOutbound, IPDivertOutbound, ProtocolOutbound, ResolvingIPOutbound, SiteDivertOutbound,
    StatReportingOutbound,
};
use crate::protocol_config::Config;
use crate::stats_server::OutboundEvent;
use cpxy_ng::geoip::find_country_code_v4;
use cpxy_ng::outbound::Outbound;
use geoip_data::CN_GEOIP;
use hickory_resolver::Resolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::xfer::Protocol;
use ipnet::Ipv4Net;
use std::borrow::Cow;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::broadcast;

pub fn cn_outbound(
    dns_servers: Vec<SocketAddr>,
    main_server: Config,
    ai_server: Option<Config>,
    tailscale_server: Option<Config>,
    events_tx: broadcast::Sender<OutboundEvent>,
) -> impl Outbound {
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
        resolver: Arc::new(
            Resolver::builder_with_config(resolver_config, TokioConnectionProvider::default())
                .build(),
        ),
    }
}

const TAILSCALE_NETWORK: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(100, 0, 0, 0), 8);

fn ip_should_route_direct(ip: Option<Ipv4Addr>) -> bool {
    match ip {
        Some(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || TAILSCALE_NETWORK.contains(&ip)
                || matches!(find_country_code_v4(&ip, CN_GEOIP), Ok(Some("CN")))
        }
        _ => true,
    }
}

fn site_should_route_ai(domain: &str) -> bool {
    domain.contains("openai.com") || domain.contains("gemini") || domain.contains("anthropic")
}

fn ip_should_route_tailscale(ip: Option<Ipv4Addr>) -> bool {
    match ip {
        Some(ip) => TAILSCALE_NETWORK.contains(&ip),
        _ => false,
    }
}
