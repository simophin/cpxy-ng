use client::dns_split::routing::should_proxy;
use client::outbound::{
    DirectOutbound, EitherOutbound, ProtocolOutbound, ProxyRoutes, Socks5Outbound,
};
use client::protocol_config::Config as ServerConfig;
use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncRead, AsyncWrite};

/// Sends private and CN destinations direct and the rest through the cpxy server or a SOCKS5
/// proxy.
pub struct Router {
    direct: DirectOutbound,
    proxy: EitherOutbound<ProtocolOutbound, Socks5Outbound>,
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
            proxy_routes,
        }
    }

    pub fn split_socks5(address: SocketAddr, proxy_routes: ProxyRoutes) -> Self {
        Self {
            direct: DirectOutbound::default(),
            proxy: EitherOutbound::Right(Socks5Outbound(address)),
            proxy_routes,
        }
    }

    fn route(&self, ip: Option<IpAddr>) -> Route {
        route(&self.proxy_routes, ip)
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
    if should_proxy(proxy_routes, ip) {
        Route::Proxy
    } else {
        Route::Direct
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
        let router = Router::split_socks5("127.0.0.1:1080".parse().unwrap(), routes);
        for ip in ["223.5.5.5", "10.1.2.3", "2400:3200::1"] {
            assert_eq!(router.route(Some(ip.parse().unwrap())), Route::Proxy);
        }
        assert_eq!(
            router.route(Some("192.168.1.1".parse().unwrap())),
            Route::Direct
        );
    }
}
