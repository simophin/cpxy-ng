use crate::either_stream::EitherStream;
use cpxy_ng::outbound::Outbound;

#[derive(Clone, Debug)]
pub enum EitherOutbound<A, B> {
    Left(A),
    Right(B),
}

impl<A, B> Outbound for EitherOutbound<A, B>
where
    A: Outbound,
    B: Outbound,
{
    async fn send(
        &self,
        req: cpxy_ng::outbound::OutboundRequest,
    ) -> anyhow::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> {
        match self {
            EitherOutbound::Left(a) => a.send(req).await.map(EitherStream::Left),
            EitherOutbound::Right(b) => b.send(req).await.map(EitherStream::Right),
        }
    }
}
