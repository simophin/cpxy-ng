use crate::routing::{Router, dial};
use anyhow::Context;
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::tls_stream::connect_tls;
use cpxy_ng::ws_stream::new_ws_stream;
use cpxy_ng::{Key, http_protocol, protocol};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use tracing::instrument;

#[instrument(ret, skip(conn, key, router), level = "info")]
pub async fn handle_connection(
    conn: impl AsyncRead + AsyncWrite + Unpin + Send,
    _from_addr: SocketAddr,
    key: Key,
    router: Arc<Router>,
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

    let route = router.route(&req.host);

    tracing::info!(
        ingress_host = req.host.as_str(),
        %route,
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
        let upstream = dial(route, req.request.host.as_str(), req.request.port).await?;

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
    use client::protocol_config::Config;
    use cpxy_ng::key_util::derive_password;
    use cpxy_ng::outbound::Outbound;
    use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_echo_server() -> SocketAddr {
        let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = echo_listener.accept().await.unwrap();
            let (mut r, mut w) = tokio::io::split(stream);
            let _ = tokio::io::copy(&mut r, &mut w).await;
        });
        echo_addr
    }

    /// Opens a tunnel through a single-connection cpxy server using `router`, with the
    /// client connecting via `server_host` (which becomes the `Host` header), then
    /// verifies an echo round-trip to `echo_addr`.
    async fn assert_tunnel_echo(router: Router, server_host: &str, echo_addr: SocketAddr) {
        let cpxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cpxy_addr = cpxy_listener.local_addr().unwrap();
        let key: Key = derive_password("integration_test_key").into();
        let router = Arc::new(router);
        tokio::spawn(async move {
            let (conn, addr) = cpxy_listener.accept().await.unwrap();
            let _ = handle_connection(conn, addr, key, router).await;
        });

        // ProtocolOutbound mirrors what client_cn does for each connection.
        let config = Config {
            host: server_host.to_string(),
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

        let msg = b"hello cpxy tunnel!";
        stream.write_all(msg).await.unwrap();
        let mut buf = vec![0u8; msg.len()];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, msg);
    }

    /// Spins up a real echo server, a real cpxy server, and a ProtocolOutbound client,
    /// then verifies data round-trips correctly through the full HTTP-upgrade →
    /// WebSocket-framing → ChaCha20-cipher stack.
    #[tokio::test]
    async fn full_tunnel_echo() {
        let echo_addr = spawn_echo_server().await;
        assert_tunnel_echo(Router::default(), "127.0.0.1", echo_addr).await;
    }

    /// A minimal single-connection SOCKS5 server. Sends the requested `host:port`
    /// on the returned channel, then relays to it.
    async fn spawn_socks5_server() -> (SocketAddr, tokio::sync::oneshot::Receiver<(String, u16)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 2];
            s.read_exact(&mut greeting).await.unwrap();
            let mut methods = vec![0u8; greeting[1] as usize];
            s.read_exact(&mut methods).await.unwrap();

            // Read the CONNECT request before replying to the greeting: this only
            // completes if the client pipelines the two.
            let mut head = [0u8; 5];
            s.read_exact(&mut head).await.unwrap();
            s.write_all(&[5, 0]).await.unwrap();
            assert_eq!(
                &head[..4],
                &[5, 1, 0, 3],
                "expected CONNECT with a domain name"
            );
            let mut host = vec![0u8; head[4] as usize];
            s.read_exact(&mut host).await.unwrap();
            let port = s.read_u16().await.unwrap();
            let host = String::from_utf8(host).unwrap();

            let mut upstream = tokio::net::TcpStream::connect((host.as_str(), port))
                .await
                .unwrap();
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            tx.send((host, port)).unwrap();
            let _ = tokio::io::copy_bidirectional(&mut s, &mut upstream).await;
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn routes_by_host_header_through_socks5() {
        let echo_addr = spawn_echo_server().await;
        let (socks_addr, requested) = spawn_socks5_server().await;
        let router = Router::new(vec![
            format!("localhost=socks5://{socks_addr}").parse().unwrap(),
        ]);

        timeout(
            Duration::from_secs(5),
            assert_tunnel_echo(router, "localhost", echo_addr),
        )
        .await
        .expect("SOCKS5 handshake should be pipelined");
        assert_eq!(
            requested.await.unwrap(),
            ("127.0.0.1".to_string(), echo_addr.port())
        );
    }

    #[tokio::test]
    async fn unmatched_host_header_connects_directly() {
        let echo_addr = spawn_echo_server().await;
        // Would fail tunnel setup if used: nothing listens on port 1.
        let router = Router::new(vec!["localhost=socks5://127.0.0.1:1".parse().unwrap()]);
        assert_tunnel_echo(router, "127.0.0.1", echo_addr).await;
    }
}
