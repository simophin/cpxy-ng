use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use std::net::Ipv4Addr;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct IPDivertOutbound<O1, O2, F> {
    pub outbound_a: Option<O1>,
    pub outbound_b: O2,
    pub should_use_a: F,
}

impl<O1, O2, F> Outbound for IPDivertOutbound<O1, O2, F>
where
    O1: Outbound + Sync,
    O2: Outbound + Sync,
    F: Fn(Option<Ipv4Addr>) -> bool + Sync,
{
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        if let Some(outbound_a) = self.outbound_a.as_ref() {
            let ip = match &req.host {
                OutboundHost::Domain(_) => None,
                OutboundHost::Resolved { ip, .. } => *ip,
            };

            let use_a = (self.should_use_a)(ip);
            tracing::debug!(
                host = req.host.host(),
                port = req.port,
                ?ip,
                use_outbound_a = use_a,
                "IPDivert routing decision"
            );

            if use_a {
                return outbound_a.send(req).await.map(EitherStream::Left);
            }
        }

        tracing::debug!(
            host = req.host.host(),
            port = req.port,
            "IPDivert routing to outbound_b"
        );
        self.outbound_b.send(req).await.map(EitherStream::Right)
    }
}
