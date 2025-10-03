use std::io::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub enum EitherStream<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> AsyncRead for EitherStream<L, R>
where
    L: AsyncRead + Unpin,
    R: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            EitherStream::Left(left) => Pin::new(left).poll_read(cx, buf),
            EitherStream::Right(right) => Pin::new(right).poll_read(cx, buf),
        }
    }
}

impl<L, R> AsyncWrite for EitherStream<L, R>
where
    L: AsyncWrite + Unpin,
    R: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        match self.get_mut() {
            EitherStream::Left(left) => Pin::new(left).poll_write(cx, buf),
            EitherStream::Right(right) => Pin::new(right).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            EitherStream::Left(left) => Pin::new(left).poll_flush(cx),
            EitherStream::Right(right) => Pin::new(right).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            EitherStream::Left(left) => Pin::new(left).poll_shutdown(cx),
            EitherStream::Right(right) => Pin::new(right).poll_shutdown(cx),
        }
    }
}
