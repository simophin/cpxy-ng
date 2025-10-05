use anyhow::Context;
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::tls_stream::connect_tls;
use cpxy_ng::{Key, http_protocol, protocol};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::instrument;

#[instrument(ret, skip(conn, key), level = "info")]
pub async fn handle_connection(
    conn: impl AsyncRead + AsyncWrite + Unpin,
    _from_addr: SocketAddr,
    key: Key,
) -> anyhow::Result<()> {
    let (req, mut conn) = match http_protocol::Request::parse(conn, &key).await {
        Ok(v) => v.take_head(),
        Err((err, mut conn)) => {
            let _ = conn
                .write_all("HTTP/1.1 404 Not Found\r\n\r\n".as_bytes())
                .await;
            return Err(err);
        }
    };

    let upstream = async {
        let upstream = TcpStream::connect((req.request.host.as_str(), req.request.port))
            .await
            .context("Error connecting to upstream")?;

        upstream
            .set_nodelay(true)
            .context("Error setting nodelay")?;

        let mut upstream =
            connect_tls(req.request.host.as_str(), req.request.tls, upstream).await?;

        tracing::debug!(
            "Writing initial plaintext: {}",
            std::str::from_utf8(&req.request.initial_plaintext).unwrap_or("<non-utf8>")
        );

        upstream
            .write_all(&req.request.initial_plaintext)
            .await
            .context("Error writing initial plaintext")?;

        // Try to read some initial data if sent
        let mut initial_response = vec![0u8; 4096];

        match timeout(
            Duration::from_millis(500),
            upstream.read(&mut initial_response),
        )
        .await
        {
            Ok(Ok(n)) => initial_response.truncate(n),
            Ok(Err(e)) => return Err(e).context("Error reading initial response from upstream"),
            Err(_) => initial_response.clear(), // Timeout
        }

        anyhow::Ok((upstream, initial_response))
    };

    match upstream.await {
        Ok((mut upstream, initial_response)) => {
            tracing::debug!("Upstream connection established");

            http_protocol::Response {
                response: protocol::Response::Success {
                    initial_response,
                    timestamp_epoch_seconds: now_epoch_seconds(),
                },
                websocket_key: req.websocket_key,
            }
            .send_over_http(&mut conn, &key)
            .await
            .context("Error sending response")?;

            let mut conn = CipherStream::new(
                conn,
                &req.request.server_send_cipher,
                &req.request.client_send_cipher,
            );

            let _ = tokio::io::copy_bidirectional(&mut upstream, &mut conn).await;
            anyhow::Ok(())
        }

        Err(e) => http_protocol::Response {
            response: protocol::Response::Error {
                msg: format!("{e:?}"),
                timestamp_epoch_seconds: now_epoch_seconds(),
            },
            websocket_key: req.websocket_key,
        }
        .send_over_http(&mut conn, &key)
        .await
        .context("Error sending response"),
    }
}
