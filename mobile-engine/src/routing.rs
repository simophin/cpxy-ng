use client::dns_split::routing::should_proxy;
use client::outbound::{
    DirectOutbound, EitherOutbound, ProtocolOutbound, ProxyRoutes, Socks5Outbound,
};
use client::protocol_config::Config as ServerConfig;
use client::region::LocalRegion;
use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncRead, AsyncWrite};

/// Which destinations go direct: private ones and the local region's, except `proxy_routes`.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub region: LocalRegion,
    pub proxy_routes: ProxyRoutes,
}

impl Policy {
    /// Forced destinations always win over regional/private direct routing.
    pub fn route(&self, ip: Option<IpAddr>) -> Route {
        if should_proxy(self.region, &self.proxy_routes, ip) {
            Route::Proxy
        } else {
            Route::Direct
        }
    }
}

/// Sends private and local-region destinations direct and the rest through the cpxy server or a
/// SOCKS5 proxy.
pub struct Router {
    direct: DirectOutbound,
    proxy: EitherOutbound<ProtocolOutbound, Socks5Outbound>,
    policy: Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    Proxy,
}

impl Router {
    pub fn new(server: ServerConfig, policy: Policy) -> Self {
        Self {
            direct: DirectOutbound::default(),
            proxy: EitherOutbound::Left(ProtocolOutbound::new(server)),
            policy,
        }
    }

    pub fn split_socks5(address: SocketAddr, policy: Policy) -> Self {
        Self {
            direct: DirectOutbound::default(),
            proxy: EitherOutbound::Right(Socks5Outbound(address)),
            policy,
        }
    }

    fn route(&self, ip: Option<IpAddr>) -> Route {
        self.policy.route(ip)
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
        let router = Router::split_socks5(
            "127.0.0.1:1080".parse().unwrap(),
            Policy {
                region: LocalRegion::Cn,
                proxy_routes: routes,
            },
        );
        for ip in ["223.5.5.5", "10.1.2.3", "2400:3200::1"] {
            assert_eq!(router.route(Some(ip.parse().unwrap())), Route::Proxy);
        }
        assert_eq!(
            router.route(Some("192.168.1.1".parse().unwrap())),
            Route::Direct
        );
    }

    #[test]
    fn overseas_region_proxies_cn_and_sends_the_rest_direct() {
        let policy = Policy {
            region: LocalRegion::Overseas,
            proxy_routes: ProxyRoutes::parse(&["8.8.4.0/24".into()]).unwrap(),
        };
        let route = |ip: &str| policy.route(Some(ip.parse().unwrap()));
        assert_eq!(route("223.5.5.5"), Route::Proxy);
        assert_eq!(route("2400:3200::1"), Route::Proxy);
        assert_eq!(route("1.1.1.1"), Route::Direct);
        assert_eq!(route("192.168.1.1"), Route::Direct);
        // Forced routes still go through the proxy.
        assert_eq!(route("8.8.4.4"), Route::Proxy);
    }
}
