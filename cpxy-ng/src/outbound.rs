use std::fmt::{Debug, Formatter};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone)]
pub enum OutboundHost {
    Domain(String),
    Resolved {
        domain: String,
        ip: Option<Ipv4Addr>,
    },
}

impl OutboundHost {
    pub fn host(&self) -> &str {
        match self {
            OutboundHost::Domain(d) => d.as_str(),
            OutboundHost::Resolved { domain: d, .. } => d.as_str(),
        }
    }

    pub async fn resolved(&mut self) -> Option<Ipv4Addr> {
        match self {
            OutboundHost::Domain(host) => {
                let ip = tokio::net::lookup_host((host.as_str(), 80))
                    .await
                    .map(|iter| {
                        iter.filter_map(|s| match s {
                            SocketAddr::V4(v4) => Some(*v4.ip()),
                            _ => None,
                        })
                        .next()
                    })
                    .unwrap_or_default();

                *self = OutboundHost::Resolved {
                    domain: std::mem::take(host),
                    ip,
                };

                ip
            }

            OutboundHost::Resolved { ip, .. } => *ip,
        }
    }
}

#[derive(Clone)]
pub struct OutboundRequest {
    pub host: OutboundHost,
    pub port: u16,
    pub tls: bool,
    pub initial_plaintext: Vec<u8>,
}

impl Debug for OutboundRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundRequest")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field(
                "initial_plaintext",
                &format!("<{} bytes>", self.initial_plaintext.len()),
            )
            .finish()
    }
}

pub trait Outbound {
    fn send(
        &self,
        req: OutboundRequest,
    ) -> impl Future<
        Output = anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static>,
    > + Send
    + Sync;
}

impl<O: Outbound> Outbound for Arc<O> {
    fn send(
        &self,
        req: OutboundRequest,
    ) -> impl Future<
        Output = anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static>,
    > + Send
    + Sync {
        self.as_ref().send(req)
    }
}
