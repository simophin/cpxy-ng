use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
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
        mut req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> {
        if let Some(outbound_a) = self.outbound_a.as_ref() {
            if (self.should_use_a)(req.host.resolved().await) {
                return outbound_a.send(req).await.map(EitherStream::Left);
            }
        }

        self.outbound_b.send(req).await.map(EitherStream::Right)
    }
}
