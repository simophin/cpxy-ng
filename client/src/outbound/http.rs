use anyhow::{Context, ensure};
use cpxy_ng::http_stream::HttpStream;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use cpxy_ng::tls_stream::connect_tls;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct HttpProxyOutbound {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl Outbound for HttpProxyOutbound {
    async fn send(
        &self,
        OutboundRequest {
            host,
            port,
            tls,
            initial_plaintext,
        }: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        let upstream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .context("failed to connect to upstream")?;
        let mut upstream = connect_tls(self.host.as_str(), self.tls, upstream)
            .await
            .context("failed to connect to upstream on tls")?;

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

        let upstream = HttpStream::parse_response(upstream, |resp| {
            ensure!(
                resp.code == Some(200),
                "upstream proxy returned non-200 status code"
            );
            anyhow::Ok(())
        })
        .await
        .map_err(|(e, _)| e)?;

        let mut upstream = connect_tls(host.host(), tls, upstream)
            .await
            .context("failed to connect to target on tls")?;

        if !initial_plaintext.is_empty() {
            upstream
                .write_all(&initial_plaintext)
                .await
                .context("failed to send initial plaintext")?;
        }

        Ok(upstream)
    }
}
