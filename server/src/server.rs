use anyhow::Context;
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::ws_stream::new_ws_stream;
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
    conn: impl AsyncRead + AsyncWrite + Unpin + Send,
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

    tracing::info!(
        target_host = req.request.host.as_str(),
        target_port = req.request.port,
        target_tls = req.request.tls,
        "Server: parsed request, connecting to upstream"
    );

    let upstream = async {
        tracing::debug!(
            host = req.request.host.as_str(),
            port = req.request.port,
            "Server: establishing TCP connection"
        );
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
            tracing::debug!("Server: upstream connection established");

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
                new_ws_stream(conn, false).await,
                &req.request.server_send_cipher,
                &req.request.client_send_cipher,
            );

            tracing::info!("Server: tunnel established, starting bidirectional copy");
            let _ = tokio::io::copy_bidirectional(&mut upstream, &mut conn).await;
            anyhow::Ok(())
        }

        Err(e) => {
            tracing::warn!(error = %e, "Server: upstream connection failed");
            http_protocol::Response {
                response: protocol::Response::Error {
                    msg: format!("{e:?}"),
                    timestamp_epoch_seconds: now_epoch_seconds(),
                },
                websocket_key: req.websocket_key,
            }
            .send_over_http(&mut conn, &key)
            .await
            .context("Error sending response")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::outbound::ProtocolOutbound;
    use cpxy_ng::outbound::Outbound;
    use client::protocol_config::Config;
    use cpxy_ng::key_util::derive_password;
    use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spins up a real echo server, a real cpxy server, and a ProtocolOutbound client,
    /// then verifies data round-trips correctly through the full HTTP-upgrade →
    /// WebSocket-framing → ChaCha20-cipher stack.
    #[tokio::test]
    async fn full_tunnel_echo() {
        // 1. TCP echo server — represents the upstream target the proxy connects to.
        let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = echo_listener.accept().await.unwrap();
            let (mut r, mut w) = tokio::io::split(stream);
            let _ = tokio::io::copy(&mut r, &mut w).await;
        });

        // 2. cpxy server — runs handle_connection for a single inbound connection.
        let cpxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cpxy_addr = cpxy_listener.local_addr().unwrap();
        let key: Key = derive_password("integration_test_key").into();
        tokio::spawn(async move {
            let (conn, addr) = cpxy_listener.accept().await.unwrap();
            let _ = handle_connection(conn, addr, key).await;
        });

        // 3. Client — ProtocolOutbound mirrors what client_cn does for each connection.
        let config = Config {
            host: "127.0.0.1".to_string(),
            port: cpxy_addr.port(),
            key: derive_password("integration_test_key").into(),
            tls: false,
        };
        let mut stream = ProtocolOutbound(config)
            .send(OutboundRequest {
                host: OutboundHost::Domain("127.0.0.1".to_string()),
                port: echo_addr.port(),
                tls: false,
                initial_plaintext: vec![],
            })
            .await
            .expect("tunnel setup failed");

        // 4. Verify echo round-trip.
        let msg = b"hello cpxy tunnel!";
        stream.write_all(msg).await.unwrap();
        let mut buf = vec![0u8; msg.len()];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, msg);
    }
}
