use client::outbound::{DirectOutbound, ProtocolOutbound, ProxyRoutes};
use client::protocol_config::Config as ServerConfig;
use client::stats_server::OutboundEvent;
use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::geoip::contains_v4;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use geoip_data::CN;
use std::borrow::Cow;
use std::net::Ipv4Addr;
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

/// TCP to private, loopback, link-local and CN addresses goes direct; everything else goes
/// through the cpxy server. Unlike `client::outbound::cn::cn_outbound`, there are no AI or
/// Tailscale branches, and nothing is resolved.
///
/// Reports each connection as an [`OutboundEvent`]: as an [`Outbound`], once it is connected;
/// through [`Router::connect`], when the caller passes the outcome to [`Router::report`], so it
/// can name the host after connecting.
pub struct Router {
    direct: DirectOutbound,
    proxy: ProtocolOutbound,
    proxy_routes: ProxyRoutes,
    events_tx: broadcast::Sender<OutboundEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    Proxy,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Proxy => "proxy",
        }
    }
}

/// How connecting went, for [`Router::report`].
pub struct Outcome {
    route: Route,
    ip: Option<Ipv4Addr>,
    port: u16,
    delay_mills: usize,
    request_time_mills: u64,
    error: Option<String>,
}

impl Router {
    pub fn new(
        server: ServerConfig,
        proxy_routes: ProxyRoutes,
        events_tx: broadcast::Sender<OutboundEvent>,
    ) -> Self {
        Self {
            direct: DirectOutbound::default(),
            proxy: ProtocolOutbound(server),
            proxy_routes,
            events_tx,
        }
    }

    fn route(&self, ip: Option<Ipv4Addr>) -> Route {
        if !self.proxy_routes.contains(ip) && ip.is_some_and(should_route_direct) {
            Route::Direct
        } else {
            Route::Proxy
        }
    }

    /// Connects without reporting it.
    pub async fn connect(
        &self,
        req: OutboundRequest,
    ) -> (
        Outcome,
        anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static>,
    ) {
        let ip = match &req.host {
            OutboundHost::Domain(_) => None,
            OutboundHost::Resolved { ip, .. } => *ip,
        };
        let route = self.route(ip);
        let port = req.port;
        let request_time_mills = SystemTime::UNIX_EPOCH
            .elapsed()
            .map_or(0, |t| t.as_millis() as u64);
        let start = Instant::now();
        let result = match route {
            Route::Direct => self.direct.send(req).await.map(EitherStream::Left),
            Route::Proxy => self.proxy.send(req).await.map(EitherStream::Right),
        };
        let outcome = Outcome {
            route,
            ip,
            port,
            delay_mills: start.elapsed().as_millis() as usize,
            request_time_mills,
            error: result.as_ref().err().map(|e| format!("{e:#}")),
        };
        (outcome, result)
    }

    /// Reports a connection made to `host`: the name it was made for, or its address.
    pub fn report(&self, outcome: Outcome, host: String) {
        let Outcome {
            route,
            ip,
            port,
            delay_mills,
            request_time_mills,
            error,
        } = outcome;
        let outbound = Cow::Borrowed(route.name());
        let event = match error {
            None => {
                tracing::debug!(%outbound, host, port, delay_mills, "Connected");
                OutboundEvent::Connected {
                    host,
                    ip,
                    port,
                    outbound,
                    delay_mills,
                    request_time_mills,
                }
            }
            Some(error) => {
                tracing::warn!(%outbound, host, port, delay_mills, error, "Connection failed");
                OutboundEvent::Error {
                    host,
                    ip,
                    port,
                    outbound,
                    delay_mills,
                    request_time_mills,
                    error,
                }
            }
        };
        let _ = self.events_tx.send(event);
    }
}

impl Outbound for Router {
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        let host = req.host.host().to_owned();
        let (outcome, result) = self.connect(req).await;
        self.report(outcome, host);
        result
    }
}

pub fn should_route_direct(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || contains_v4(&ip, CN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixed_routes_override_direct_addresses() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("http://:key@{}", closed.local_addr().unwrap())
            .parse()
            .unwrap();
        drop(closed);
        let (tx, mut rx) = broadcast::channel(4);
        let outbound = Router::new(
            server,
            ProxyRoutes::parse(&["223.5.5.5/32".into(), "10.0.0.0/8".into()]).unwrap(),
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
                matches!(rx.recv().await.unwrap(), OutboundEvent::Error { outbound, ip: Some(addr), .. } if outbound == "proxy" && addr.to_string() == ip)
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
