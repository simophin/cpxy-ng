use crate::handshaker::Handshaker;
use cpxy_ng::http_proxy::{ProxyRequest, parse_http_proxy_stream};
use cpxy_ng::http_stream::HttpStream;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

pub struct HttpProxyHandshaker<S> {
    stream: HttpStream<(), S>,
    is_tunnel: bool,
}

impl<S> Handshaker<S> for HttpProxyHandshaker<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    type StreamType = HttpStream<(), S>;
    type RequestType = ProxyRequest;

    async fn accept(stream: S) -> anyhow::Result<(ProxyRequest, HttpProxyHandshaker<S>)> {
        let (req, stream) = parse_http_proxy_stream(stream)
            .await
            .map_err(|(e, _)| e)?
            .take_head();

        let is_tunnel = matches!(&req, ProxyRequest::Socket(..));
        Ok((req, HttpProxyHandshaker { stream, is_tunnel }))
    }

    async fn respond_ok(mut self) -> anyhow::Result<HttpStream<(), S>> {
        if self.is_tunnel {
            self.stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }

        Ok(self.stream)
    }

    async fn respond_err(mut self, msg: &str) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        self.stream
            .write_all(construct_error_http_response(500, msg).as_bytes())
            .await?;
        Ok(())
    }

    fn stream_mut(&mut self) -> &mut HttpStream<(), S> {
        &mut self.stream
    }
}

pub fn construct_error_http_response(code: u16, msg: &str) -> String {
    format!(
        "HTTP/1.1 {code} Internal Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        msg.as_bytes().len(),
        msg
    )
}
