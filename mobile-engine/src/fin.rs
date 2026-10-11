//! Works around ipstack sending no FIN when the outbound side closes while data is in flight.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Sleep;

/// How often shutting down is retried while it waits.
const RETRY: Duration = Duration::from_millis(20);

/// An ipstack TCP stream whose shutdown is polled again every [`RETRY`] while it waits.
///
/// ipstack only sends the FIN from `poll_shutdown` once every segment sent has been acknowledged,
/// and nothing wakes the task when the last acknowledgement arrives. A server closing right after
/// its last bytes, as most do, left the app waiting for the end of the stream forever.
pub struct FinRetry<S> {
    inner: S,
    retry: Option<Pin<Box<Sleep>>>,
}

impl<S> FinRetry<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, retry: None }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for FinRetry<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for FinRetry<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            if let Poll::Ready(result) = Pin::new(&mut this.inner).poll_shutdown(cx) {
                this.retry = None;
                return Poll::Ready(result);
            }
            let retry = this
                .retry
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(RETRY)));
            if retry.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            retry.as_mut().reset(tokio::time::Instant::now() + RETRY);
        }
    }
}
