use hickory_resolver::config::ResolverConfig;
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::{Resolver, TokioResolver};
use std::fmt::{Debug, Formatter};
use std::net::Ipv4Addr;
use std::sync::{Arc, LazyLock};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone)]
pub enum OutboundHost {
    Domain(String),
    Resolved {
        domain: String,
        ip: Option<Ipv4Addr>,
    },
}

static RESOLVER: LazyLock<TokioResolver> = LazyLock::new(|| {
    Resolver::builder_with_config(
        ResolverConfig::cloudflare(),
        TokioConnectionProvider::default(),
    )
    .build()
});

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
                let ip = RESOLVER
                    .ipv4_lookup(host.as_str())
                    .await
                    .ok()
                    .and_then(|r| r.iter().next().copied())
                    .map(|r| r.0);

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
    ) -> impl Future<Output = anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static>> + Send;
}

impl<O: Outbound> Outbound for Arc<O> {
    fn send(
        &self,
        req: OutboundRequest,
    ) -> impl Future<Output = anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static>> + Send
    {
        self.as_ref().send(req)
    }
}
