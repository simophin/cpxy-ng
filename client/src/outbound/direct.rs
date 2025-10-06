use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use cpxy_ng::tls_stream::connect_tls;
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
        let upstream = match &host {
            OutboundHost::Resolved { ip: Some(ip), .. } => TcpStream::connect((*ip, port))
                .await
                .with_context(|| format!("Failed to connect to {ip}:{port}"))?,
            OutboundHost::Domain(host) | OutboundHost::Resolved { domain: host, .. } => {
                TcpStream::connect((host.as_str(), port))
                    .await
                    .with_context(|| format!("Failed to connect to {host}:{port}"))?
            }
        };

        let mut upstream = connect_tls(host.host(), tls, upstream).await?;

        if !initial_plaintext.is_empty() {
            upstream
                .write_all(&initial_plaintext)
                .await
                .context("Error sending initial payload to remote")?;
        }

        anyhow::Ok(upstream)
    }
}
