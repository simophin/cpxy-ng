use bytes::{Buf, Bytes};
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pin_project! {
    pub struct HttpStream<H, S> {
        head: H,
        parse_remnant: Bytes,
        #[pin]
        inner: S,
    }
}

impl<H, S: AsyncRead + Unpin> HttpStream<H, S> {
    pub async fn parse_response(
        mut inner: S,
        parser: impl FnMut(&httparse::Response<'_, '_>) -> anyhow::Result<H>,
    ) -> anyhow::Result<Self>
    where
        S: AsyncRead + Unpin,
    {
        let (head, parse_remnant) =
            super::http_util::parse_http_response(&mut inner, parser).await?;

        Ok(Self {
            head,
            parse_remnant,
            inner,
        })
    }

    pub async fn parse_request(
        mut inner: S,
        parser: impl FnMut(&httparse::Request<'_, '_>) -> anyhow::Result<H>,
    ) -> anyhow::Result<Self>
    where
        S: AsyncRead + Unpin,
    {
        let (head, parse_remnant) =
            super::http_util::parse_http_request(&mut inner, parser).await?;

        Ok(Self {
            head,
            parse_remnant,
            inner,
        })
    }
}

impl<H, S> HttpStream<H, S> {
    pub fn head(&self) -> &H {
        &self.head
    }

    pub fn take_head(self) -> (H, HttpStream<(), S>) {
        let Self {
            head,
            parse_remnant,
            inner,
        } = self;

        (
            head,
            HttpStream {
                head: (),
                parse_remnant,
                inner,
            },
        )
    }
}

impl<H, S: AsyncRead> AsyncRead for HttpStream<H, S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();
        if this.parse_remnant.has_remaining() {
            let bytes_read = buf.remaining().min(this.parse_remnant.remaining());
            buf.put_slice(&this.parse_remnant[..bytes_read]);
            this.parse_remnant.advance(bytes_read);
            cx.waker().wake_by_ref();
            return Poll::Ready(Ok(()));
        }

        this.inner.poll_read(cx, buf)
    }
}

impl<H, S: AsyncWrite> AsyncWrite for HttpStream<H, S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().inner.poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tokio::io::{AsyncReadExt, BufReader};

    use crate::http_util::HttpHeaderExt;

    use super::*;

    #[tokio::test]
    async fn http_request_parsing_works() {
        let request_text = b"GET /get HTTP/1.1\r\nHost: example.com\r\n\r\nHello, world";

        let req = HttpStream::parse_request(Cursor::new(request_text), |req| {
            assert_eq!(req.method, Some("GET"));
            assert_eq!(req.path, Some("/get"));
            assert_eq!(req.version, Some(1));
            assert_eq!(req.headers.len(), 1);
            assert_eq!(
                req.headers.get_header_value("Host").unwrap(),
                b"example.com"
            );
            Ok(())
        })
        .await
        .expect("Parsing request fails");

        let mut actual_read = vec![];
        BufReader::new(req)
            .read_to_end(&mut actual_read)
            .await
            .expect("To read until the end");
        assert_eq!(actual_read, b"Hello, world");
    }

    #[tokio::test]
    async fn http_response_parsing_works() {
        let response_text = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nHello, world";

        let res: HttpStream<(), Cursor<&'static [u8; 51]>> =
            HttpStream::parse_response(Cursor::new(response_text), |res| {
                assert_eq!(res.code, Some(200));
                assert_eq!(res.version, Some(1));
                assert_eq!(res.headers.len(), 1);
                assert_eq!(
                    res.headers.get_header_value("Content-Length").unwrap(),
                    b"13"
                );
                Ok(())
            })
            .await
            .expect("Parsing response fails");

        let mut actual_read = vec![];
        BufReader::new(res)
            .read_to_end(&mut actual_read)
            .await
            .expect("To read until the end");
        assert_eq!(actual_read, b"Hello, world");
    }
}
