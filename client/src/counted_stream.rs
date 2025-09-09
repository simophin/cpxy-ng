use std::sync::{Arc, atomic::AtomicUsize};

use tokio::io::{AsyncRead, AsyncWrite};

pub struct CountedStream<S> {
    stream: S,
    bytes_received: Arc<AtomicUsize>,
    bytes_sent: Arc<AtomicUsize>,
}

impl<S> AsyncRead for CountedStream<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let pre_len = buf.filled().len();
        let pin = std::pin::Pin::new(&mut this.stream);
        let poll = pin.poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &poll {
            let post_len = buf.filled().len();
            let read_bytes = post_len - pre_len;
            this.bytes_received
                .fetch_add(read_bytes, std::sync::atomic::Ordering::Relaxed);
        }
        poll
    }
}

impl<S> AsyncWrite for CountedStream<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let pin = std::pin::Pin::new(&mut this.stream);
        let poll = pin.poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(written_bytes)) = &poll {
            this.bytes_sent
                .fetch_add(*written_bytes, std::sync::atomic::Ordering::Relaxed);
        }
        poll
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let pin = std::pin::Pin::new(&mut this.stream);
        pin.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let pin = std::pin::Pin::new(&mut this.stream);
        pin.poll_shutdown(cx)
    }
}

impl<S> CountedStream<S> {
    pub fn new(stream: S, bytes_received: Arc<AtomicUsize>, bytes_sent: Arc<AtomicUsize>) -> Self {
        Self {
            stream,
            bytes_received,
            bytes_sent,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        join,
    };

    use super::*;

    #[tokio::test]
    async fn counted_stream_works() {
        let bytes_received = Arc::new(AtomicUsize::new(0));
        let bytes_sent = Arc::new(AtomicUsize::new(0));

        let (client, mut server) = tokio::io::duplex(64);

        let echo_from_server = async {
            let mut buf = [0u8; 16];
            loop {
                let n = server.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                server.write_all(&buf[..n]).await.unwrap();
            }
        };

        let mut client = CountedStream::new(client, bytes_received.clone(), bytes_sent.clone());
        let msg = b"Hello, world!";

        let send_from_client = async move {
            client.write_all(msg).await.unwrap();
            let mut buf = vec![0u8; msg.len()];
            client.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, msg);

            drop(client);
        };

        join!(echo_from_server, send_from_client);

        assert_eq!(
            bytes_sent.load(std::sync::atomic::Ordering::Relaxed),
            msg.len()
        );
        assert_eq!(
            bytes_received.load(std::sync::atomic::Ordering::Relaxed),
            msg.len()
        );
    }
}
