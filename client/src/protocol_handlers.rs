use super::protocol_config::Config as ProtocolConfig;
use crate::handshaker::Handshaker;
use anyhow::{Context, bail};
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::http_protocol::Request as ProtocolRequest;
use cpxy_ng::tls_stream::TlsClientStream;
use cpxy_ng::{http_protocol, protocol};
use std::io::Cursor;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, join, split};
use tokio::io::{AsyncWriteExt, copy_bidirectional};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::instrument;

#[instrument]
pub async fn send_protocol_request(
    config: &ProtocolConfig,
    req: ProtocolRequest,
) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static> {
    let conn = TcpStream::connect((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("Error connecting to upstream server: {config:?}"))?;

    let mut conn = if config.tls {
        TlsClientStream::connect_tls(config.host.as_str(), conn)
            .await
            .context("Error establishing TLS connection to upstream server")?
    } else {
        TlsClientStream::Plain(conn)
    };

    req.send_over_http(&mut conn, &config.key)
        .await
        .context("Error sending request to upstream server")?;

    let (resp, conn) = http_protocol::Response::parse(conn, &config.key)
        .await
        .map_err(|(e, _)| e)?
        .take_head();

    match resp.response {
        protocol::Response::Success {
            initial_response, ..
        } => {
            let (r, w) = split(CipherStream::new(
                conn,
                &req.request.client_send_cipher,
                &req.request.server_send_cipher,
            ));

            let r = Cursor::new(initial_response).chain(r);
            Ok(join(r, w))
        }
        protocol::Response::Error { msg, .. } => bail!("Error response from server: {msg}"),
    }
}

pub async fn serve<S, HS, F, Fut, US>(stream: S, send_upstream: F) -> anyhow::Result<()>
where
    HS: Handshaker<S>,
    <HS as Handshaker<S>>::RequestType: Into<protocol::Request>,
    <HS as Handshaker<S>>::StreamType: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(protocol::Request) -> Fut,
    Fut: Future<Output = anyhow::Result<(US, protocol::Response)>>,
    US: AsyncRead + AsyncWrite + Unpin + 'static,
{
    let (req, handshake) = HS::accept(stream).await?;

    let read_initial_data = HS::can_read_initial_data(&req);
    let mut req: protocol::Request = req.into();
    let mut conn: <HS as Handshaker<S>>::StreamType;
    let mut upstream: US;

    if read_initial_data {
        conn = handshake
            .respond_ok()
            .await
            .context("Error responding ok")?;

        let mut buf = vec![0u8; 4096];
        let n = timeout(Duration::from_millis(200), conn.read(&mut buf))
            .await
            .unwrap_or(Ok(0))
            .context("Error reading initial data")?;

        req.initial_plaintext.extend_from_slice(&buf[..n]);
        match send_upstream(req).await? {
            (
                up,
                protocol::Response::Success {
                    initial_response, ..
                },
            ) => {
                upstream = up;
                conn.write_all(&initial_response)
                    .await
                    .context("Error writing initial response to client")?;
            }

            (_, protocol::Response::Error { msg, .. }) => {
                bail!("Error response from server: {msg}");
            }
        }
    } else {
        match send_upstream(req).await {
            Ok((
                up,
                protocol::Response::Success {
                    initial_response, ..
                },
            )) => {
                upstream = up;
                conn = handshake
                    .respond_ok()
                    .await
                    .context("Error responding ok")?;

                conn.write_all(&initial_response)
                    .await
                    .context("Error writing initial response to client")?;
            }

            Ok((_, protocol::Response::Error { msg, .. })) => {
                handshake
                    .respond_err(&msg)
                    .await
                    .context("Error responding err")?;
                return Ok(());
            }

            Err(e) => {
                handshake
                    .respond_err(&format!("Error sending upstream: {e}"))
                    .await
                    .context("Error responding err")?;
                return Err(e);
            }
        }
    }

    let _ = copy_bidirectional(&mut conn, &mut upstream).await;
    Ok(())
}
