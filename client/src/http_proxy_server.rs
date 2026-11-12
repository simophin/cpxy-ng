use crate::handshaker::Handshaker;
use cpxy_ng::http_proxy::{MethodNotAllowed, ProxyRequest, parse_http_proxy_stream};
use cpxy_ng::http_stream::HttpStream;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Accepts HTTP `CONNECT` tunnels only.
pub struct HttpProxyHandshaker<S> {
    stream: HttpStream<(), S>,
}

impl<S> Handshaker<S> for HttpProxyHandshaker<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    type StreamType = HttpStream<(), S>;
    type RequestType = ProxyRequest;

    async fn accept(stream: S) -> anyhow::Result<(ProxyRequest, HttpProxyHandshaker<S>)> {
        let (req, stream) = match parse_http_proxy_stream(stream).await {
            Ok(s) => s.take_head(),
            Err((e, mut stream)) => {
                let (code, reason) = if e.downcast_ref::<MethodNotAllowed>().is_some() {
                    (405, "Method Not Allowed")
                } else {
                    (400, "Bad Request")
                };
                let _ = stream
                    .write_all(error_http_response(code, reason, &e.to_string()).as_bytes())
                    .await;
                return Err(e);
            }
        };

        Ok((req, HttpProxyHandshaker { stream }))
    }

    async fn respond_ok(mut self) -> anyhow::Result<HttpStream<(), S>> {
        self.stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        Ok(self.stream)
    }

    async fn respond_err(mut self, msg: &str) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        self.stream
            .write_all(error_http_response(502, "Bad Gateway", msg).as_bytes())
            .await?;
        Ok(())
    }

    fn stream_mut(&mut self) -> &mut HttpStream<(), S> {
        &mut self.stream
    }
}

fn error_http_response(code: u16, reason: &str, msg: &str) -> String {
    let allow = if code == 405 {
        "Allow: CONNECT\r\n"
    } else {
        ""
    };
    format!(
        "HTTP/1.1 {code} {reason}\r\n{allow}Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
        msg.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn rejects_forward_proxy_with_405() {
        let (mut client, server) = tokio::io::duplex(1024);
        client
            .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .await
            .unwrap();

        assert!(HttpProxyHandshaker::accept(server).await.is_err());

        let mut resp = String::new();
        client.read_to_string(&mut resp).await.unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
            "{resp}"
        );
        assert!(resp.contains("Allow: CONNECT\r\n"), "{resp}");
    }
}
