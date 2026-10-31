use client::dns_split::is_local_region_ip;
use client::outbound::{
    DirectOutbound, EitherOutbound, ProtocolOutbound, ProxyRoutes, Socks5Outbound, is_private,
};
use client::protocol_config::Config as ServerConfig;
use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncRead, AsyncWrite};

/// Split routing is retained for legacy consumers and Android's conservative capture fallback.
/// OpenWrt's exact kernel routes use unconditional SOCKS forwarding instead.
pub struct Router {
    direct: DirectOutbound,
    proxy: EitherOutbound<ProtocolOutbound, Socks5Outbound>,
    always_proxy: bool,
    proxy_routes: ProxyRoutes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    Proxy,
}

impl Router {
    pub fn new(server: ServerConfig, proxy_routes: ProxyRoutes) -> Self {
        Self {
            direct: DirectOutbound::default(),
            proxy: EitherOutbound::Left(ProtocolOutbound(server)),
            always_proxy: false,
            proxy_routes,
        }
    }

    pub fn socks5(address: SocketAddr) -> Self {
        Self {
            direct: DirectOutbound::default(),
            proxy: EitherOutbound::Right(Socks5Outbound(address)),
            always_proxy: true,
            proxy_routes: ProxyRoutes::default(),
        }
    }

    pub fn split_socks5(address: SocketAddr, proxy_routes: ProxyRoutes) -> Self {
        Self {
            proxy_routes,
            always_proxy: false,
            ..Self::socks5(address)
        }
    }

    fn route(&self, ip: Option<IpAddr>) -> Route {
        if self.always_proxy {
            Route::Proxy
        } else {
            route(&self.proxy_routes, ip)
        }
    }
}

impl Outbound for Router {
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        let ip = match &req.host {
            OutboundHost::Domain(_) => None,
            OutboundHost::Resolved { ip, .. } => *ip,
        };
        match self.route(ip) {
            Route::Direct => self.direct.send(req).await.map(EitherStream::Left),
            Route::Proxy => self.proxy.send(req).await.map(EitherStream::Right),
        }
    }
}

/// Forced destinations always win over regional/private direct routing.
pub fn route(proxy_routes: &ProxyRoutes, ip: Option<IpAddr>) -> Route {
    if !proxy_routes.contains(ip) && ip.is_some_and(should_route_direct) {
        Route::Direct
    } else {
        Route::Proxy
    }
}

pub fn should_route_direct(ip: IpAddr) -> bool {
    is_private(ip) || is_local_region_ip(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_routes_override_direct_addresses() {
        let routes = ProxyRoutes::parse(&[
            "223.5.5.5/32".into(),
            "10.0.0.0/8".into(),
            "2400:3200::/32".into(),
        ])
        .unwrap();
        let router = Router::split_socks5("127.0.0.1:1080".parse().unwrap(), routes);
        for ip in ["223.5.5.5", "10.1.2.3", "2400:3200::1"] {
            assert_eq!(router.route(Some(ip.parse().unwrap())), Route::Proxy);
        }
        assert_eq!(
            router.route(Some("192.168.1.1".parse().unwrap())),
            Route::Direct
        );
    }

    #[test]
    fn router_tun_never_reclassifies_a_private_destination_as_direct() {
        let router = Router::socks5("127.0.0.1:1080".parse().unwrap());
        for ip in ["127.0.0.1", "10.1.2.3", "::1"] {
            assert_eq!(router.route(Some(ip.parse().unwrap())), Route::Proxy);
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
