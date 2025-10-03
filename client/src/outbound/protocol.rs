use crate::protocol_config::Config;
use anyhow::{Context, bail};
use cpxy_ng::cipher_select::select_cipher_based_on_port;
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::key_util::random_vec;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use cpxy_ng::tls_stream::TlsClientStream;
use cpxy_ng::{http_protocol, protocol};
use std::io::Cursor;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::{TcpStream, lookup_host};

#[derive(Debug)]
pub struct ProtocolOutbound(pub Config);

impl Outbound for ProtocolOutbound {
    #[tracing::instrument(
        skip(initial_plaintext),
        name = "send_protocol_outbound",
        level = "info"
    )]
    async fn send(
        &self,
        OutboundRequest {
            host,
            port,
            tls,
            initial_plaintext,
        }: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> {
        let config = &self.0;
        let addr = lookup_host((config.host.as_str(), config.port))
            .await
            .with_context(|| format!("failed to lookup host {:?}", config.host))?
            .filter(|addr| addr.is_ipv4())
            .next()
            .with_context(|| {
                format!(
                    "failed to lookup host {:?}: no ipv4 address available",
                    config.host
                )
            })?;

        let conn = TcpStream::connect(addr)
            .await
            .with_context(|| format!("Error connecting to upstream server: {config:?}"))?;

        conn.set_nodelay(true)
            .context("Error setting nodelay on TCP stream")?;

        let mut conn = if config.tls {
            TlsClientStream::connect_tls(config.host.as_str(), conn)
                .await
                .context("Error establishing TLS connection to upstream server")?
        } else {
            TlsClientStream::Plain(conn)
        };

        let (client_send_cipher, server_send_cipher) = select_cipher_based_on_port(port);

        let req = http_protocol::Request {
            request: protocol::Request {
                host,
                port,
                tls,
                client_send_cipher: client_send_cipher.clone(),
                server_send_cipher: server_send_cipher.clone(),
                initial_plaintext,
                timestamp_epoch_seconds: 0,
            },
            websocket_key: random_vec(12),
            host: config.host.clone(),
        };

        req.send_over_http(&mut conn, &config.key)
            .await
            .context("Error sending request to upstream server")?;

        let (http_protocol::Response { response, .. }, conn) =
            http_protocol::Response::parse(conn, &config.key)
                .await
                .map_err(|(e, _)| e)?
                .take_head();

        match response {
            protocol::Response::Success {
                initial_response, ..
            } => {
                tracing::debug!(
                    "Server respond successfully with {} bytes of initial response",
                    initial_response.len()
                );
                let (r, w) = tokio::io::split(CipherStream::new(
                    conn,
                    &client_send_cipher,
                    &server_send_cipher,
                ));

                let r = Cursor::new(initial_response).chain(r);
                Ok(tokio::io::join(r, w))
            }
            protocol::Response::Error { msg, .. } => {
                tracing::info!("Server responded with error: {msg}");
                bail!("Error from server: {msg}")
            }
        }
    }
}
