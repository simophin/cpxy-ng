use super::warm_pool::WarmPool;
use crate::protocol_config::Config;
use anyhow::Context;
use cpxy_ng::cipher_select::select_cipher_based_on_port;
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::key_util::random_vec;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use cpxy_ng::ws_stream::new_ws_stream;
use cpxy_ng::{http_protocol, protocol};
use std::fmt::{Display, Formatter};
use std::io::Cursor;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;

/// Connects through a cpxy server. Clones share one pool of pre-connected sockets, see
/// [`WarmPool`].
#[derive(Debug, Clone)]
pub struct ProtocolOutbound {
    config: Config,
    pool: Arc<WarmPool>,
}

impl ProtocolOutbound {
    pub fn new(config: Config) -> Self {
        let pool = WarmPool::new(config.host.clone(), config.port);
        Self { config, pool }
    }
}

/// The server answered the request with an error, e.g. because the target is unreachable.
/// Retrying on another connection won't help.
#[derive(Debug)]
struct Rejected(String);

impl Display for Rejected {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Error from server: {}", self.0)
    }
}

impl std::error::Error for Rejected {}

impl Outbound for ProtocolOutbound {
    #[tracing::instrument(skip(self, req), name = "send_protocol_outbound", level = "info")]
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        let config = &self.config;
        tracing::info!(
            server_host = config.host.as_str(),
            server_port = config.port,
            target_host = req.host.host(),
            target_port = req.port,
            target_tls = req.tls,
            "ProtocolOutbound: connecting to cpxy server"
        );

        if let Some(conn) = self.pool.take() {
            tracing::debug!("ProtocolOutbound: using a pre-connected socket");
            match self.request(conn, &req).await {
                Ok(stream) => return Ok(stream),
                Err(e) if e.downcast_ref::<Rejected>().is_some() => return Err(e),
                // The connection may have died while idle, before the server read the request.
                Err(e) => tracing::debug!(
                    error = %e,
                    "ProtocolOutbound: pre-connected socket failed, retrying on a fresh one"
                ),
            }
        }

        let conn = self.pool.connect().await?;
        self.request(conn, &req).await
    }
}

impl ProtocolOutbound {
    /// Sends the cpxy request on `conn` and, once the server accepts it, returns the tunnel.
    async fn request(
        &self,
        mut conn: TcpStream,
        req: &OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static + use<>> {
        let config = &self.config;
        let (client_send_cipher, server_send_cipher) = select_cipher_based_on_port(req.port);

        let http_req = http_protocol::Request {
            request: protocol::Request {
                host: req.host.host().to_string(),
                port: req.port,
                tls: req.tls,
                client_send_cipher: client_send_cipher.clone(),
                server_send_cipher: server_send_cipher.clone(),
                initial_plaintext: req.initial_plaintext.clone(),
                timestamp_epoch_seconds: 0,
            },
            websocket_key: random_vec(12),
            host: config.host.clone(),
        };

        tracing::debug!("ProtocolOutbound: sending request to server");
        http_req
            .send_over_http(&mut conn, &config.key)
            .await
            .context("Error sending request to upstream server")?;

        tracing::debug!("ProtocolOutbound: waiting for server response");
        let (http_protocol::Response { response, .. }, conn) =
            http_protocol::Response::parse(conn, &config.key)
                .await
                .map_err(|(e, _)| e)?
                .take_head();

        match response {
            protocol::Response::Success {
                initial_response, ..
            } => {
                tracing::info!("ProtocolOutbound: server accepted connection");
                let (r, w) = tokio::io::split(CipherStream::new(
                    new_ws_stream(conn, true).await,
                    &client_send_cipher,
                    &server_send_cipher,
                ));

                let r = Cursor::new(initial_response).chain(r);
                Ok(tokio::io::join(r, w))
            }
            protocol::Response::Error { msg, .. } => {
                tracing::warn!(msg, "ProtocolOutbound: server rejected connection");
                Err(Rejected(msg).into())
            }
        }
    }
}
