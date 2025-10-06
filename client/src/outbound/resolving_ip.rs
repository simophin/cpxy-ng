use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use hickory_resolver::TokioResolver;
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
            let ip = self
                .resolver
                .ipv4_lookup(host.as_str())
                .await
                .map(|lookup| lookup.iter().next().map(|ip| ip.0))
                .unwrap_or_else(|e| {
                    tracing::error!(?e, "failed to resolve domain: {host}");
                    None
                });

            req.host = OutboundHost::Resolved {
                domain: std::mem::take(host),
                ip,
            };
        }

        self.inner.send(req).await
    }
}
