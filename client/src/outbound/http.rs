use anyhow::{Context, ensure};
use cpxy_ng::http_stream::HttpStream;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct HttpProxyOutbound {
    pub host: String,
    pub port: u16,
}

impl Outbound for HttpProxyOutbound {
    async fn send(
        &self,
        OutboundRequest {
            host,
            port,
            initial_plaintext,
        }: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        tracing::info!(
            proxy_host = self.host,
            proxy_port = self.port,
            "HttpProxyOutbound: connecting to proxy"
        );
        let mut upstream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .context("failed to connect to upstream")?;

        tracing::debug!(
            target = host.host(),
            target_port = port,
            "HttpProxyOutbound: sending CONNECT request"
        );
        upstream
            .write_all(
                format!(
                    "CONNECT {}:{port} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
                    host.host(),
                    self.host,
                    self.port
                )
                .as_bytes(),
            )
            .await
            .context("failed to send CONNECT request")?;

        let mut upstream = HttpStream::parse_response(upstream, |resp| {
            ensure!(
                resp.code == Some(200),
                "upstream proxy returned non-200 status code"
            );
            anyhow::Ok(())
        })
        .await
        .map_err(|(e, _)| e)?;

        tracing::debug!("HttpProxyOutbound: proxy CONNECT accepted");

        if !initial_plaintext.is_empty() {
            upstream
                .write_all(&initial_plaintext)
                .await
                .context("failed to send initial plaintext")?;
        }

        tracing::info!(
            target = host.host(),
            target_port = port,
            "HttpProxyOutbound: connection established"
        );
        Ok(upstream)
    }
}
