use crate::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use tokio::io::{AsyncRead, AsyncWrite};

pub struct SiteDivertOutbound<O1, O2, F> {
    pub outbound_a: Option<O1>,
    pub outbound_b: O2,
    pub should_use_a: F,
}

impl<O1, O2, F> Outbound for SiteDivertOutbound<O1, O2, F>
where
    O1: Outbound + Sync,
    O2: Outbound + Sync,
    F: Fn(&str) -> bool + Sync,
{
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static> {
        match self.outbound_a.as_ref() {
            Some(a) if (self.should_use_a)(&req.host) => a.send(req).await.map(EitherStream::Left),
            _ => self.outbound_b.send(req).await.map(EitherStream::Right),
        }
    }
}
