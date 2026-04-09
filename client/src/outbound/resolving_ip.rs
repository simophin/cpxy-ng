use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use hickory_resolver::TokioResolver;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct ResolvingIPOutbound<O> {
    pub inner: O,
    pub resolver: Arc<TokioResolver>,
}

impl<O> Outbound for ResolvingIPOutbound<O>
where
    O: Outbound + Sync,
{
    async fn send(
        &self,
        mut req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        if let OutboundHost::Domain(host) = &mut req.host {
            let mut ip: Option<Ipv4Addr> = host.parse().ok();

            if ip.is_none() {
                tracing::debug!(host, "Resolving domain");
                ip = self
                    .resolver
                    .ipv4_lookup(host.as_str())
                    .await
                    .map(|lookup| lookup.iter().next().map(|ip| ip.0))
                    .unwrap_or_else(|e| {
                        tracing::error!(?e, host, "Failed to resolve domain");
                        None
                    });
            }

            tracing::debug!(host = host.as_str(), ?ip, "DNS resolution result");
            req.host = OutboundHost::Resolved {
                domain: std::mem::take(host),
                ip,
            };
        }

        self.inner.send(req).await
    }
}
