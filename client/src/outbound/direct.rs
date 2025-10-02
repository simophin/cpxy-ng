use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use cpxy_ng::tls_stream::TlsClientStream;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::instrument;

#[derive(Debug, Clone)]
pub struct DirectOutbound;

impl Outbound for DirectOutbound {
    #[instrument(skip(self, initial_plaintext), name = "send_direct_outbound")]
    async fn send(
        &self,
        OutboundRequest {
            host,
            port,
            tls,
            initial_plaintext,
        }: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> {
        let upstream = TcpStream::connect((host.as_str(), port))
            .await
            .context("Error connecting to remote")?;

        let mut upstream = if tls {
            TlsClientStream::connect_tls(host.as_str(), upstream)
                .await
                .context("Error establishing TLS connection to remote")?
        } else {
            TlsClientStream::Plain(upstream)
        };

        upstream
            .write_all(&initial_plaintext)
            .await
            .context("Error sending initial payload to remote")?;

        anyhow::Ok(upstream)
    }
}
