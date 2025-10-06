use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use tokio::io::{AsyncRead, AsyncWrite};

pub enum EitherOutbound<A, B> {
    Left(A),
    Right(B),
}

impl<A, B> Outbound for EitherOutbound<A, B>
where
    A: Outbound + Sync,
    B: Outbound + Sync,
{
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        match self {
            EitherOutbound::Left(a) => a.send(req).await.map(EitherStream::Left),
            EitherOutbound::Right(b) => b.send(req).await.map(EitherStream::Right),
        }
    }
}
