use crate::outbound::{DirectOutbound, IPDivertOutbound, ProtocolOutbound, SiteDivertOutbound};
use crate::protocol_config::Config;
use cpxy_ng::geoip::find_country_code_v4;
use geoip_data::CN_GEOIP;
use ipnet::Ipv4Net;
use std::net::Ipv4Addr;

pub fn cn_outbound(
    main_server: Config,
    ai_server: Option<Config>,
    tailscale_server: Option<Config>,
) -> IPDivertOutbound<
    ProtocolOutbound,
    SiteDivertOutbound<
        ProtocolOutbound,
        IPDivertOutbound<DirectOutbound, ProtocolOutbound, fn(Ipv4Addr) -> bool>,
        fn(&str) -> bool,
    >,
    fn(Ipv4Addr) -> bool,
> {
    let global_outbound = ProtocolOutbound(main_server);
    let ai_outbound = ai_server.map(ProtocolOutbound);
    let tailscale_outbound = tailscale_server.map(ProtocolOutbound);

    IPDivertOutbound {
        outbound_a: tailscale_outbound,
        outbound_b: SiteDivertOutbound {
            outbound_a: ai_outbound,
            outbound_b: IPDivertOutbound {
                outbound_a: Some(DirectOutbound),
                outbound_b: global_outbound,
                should_use_a: ip_should_route_direct,
            },
            should_use_a: site_should_route_ai,
        },
        should_use_a: ip_should_route_tailscale,
    }
}

const TAILSCALE_NETWORK: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(100, 0, 0, 0), 8);

fn ip_should_route_direct(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || TAILSCALE_NETWORK.contains(&ip)
        || matches!(find_country_code_v4(&ip, CN_GEOIP), Ok(Some("CN")))
}

fn site_should_route_ai(domain: &str) -> bool {
    AI_DOMAIN_POSTFIXES
        .iter()
        .any(|postfix| domain.to_ascii_lowercase().ends_with(postfix))
}

fn ip_should_route_tailscale(ip: Ipv4Addr) -> bool {
    TAILSCALE_NETWORK.contains(&ip)
}

const AI_DOMAIN_POSTFIXES: &[&str] = &["anthropic.com", "openai.com", "chatgpt.com"];
