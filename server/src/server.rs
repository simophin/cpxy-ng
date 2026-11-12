use crate::routing::Router;
use anyhow::Context;
use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::ws_stream::new_ws_stream;
use cpxy_ng::{Key, http_protocol, protocol};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
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
        "Server: parsed request, connecting to upstream"
    );

    let upstream = async {
        // Older clients set `tls` to have the server wrap the target connection in TLS, which
        // is no longer supported. Refuse instead of sending their plaintext to a TLS port.
        anyhow::ensure!(
            !req.request.tls,
            "TLS to the target is no longer supported, upgrade the client"
        );

        tracing::debug!(
            host = req.request.host.as_str(),
            port = req.request.port,
            "Server: establishing TCP connection"
        );
        let mut upstream = router
            .dial(route, req.request.host.as_str(), req.request.port)
            .await?;

        tracing::debug!(
            "Writing initial plaintext: {}",
            std::str::from_utf8(&req.request.initial_plaintext).unwrap_or("<non-utf8>")
        );

        upstream
            .write_all(&req.request.initial_plaintext)
            .await
            .context("Error writing initial plaintext")?;

        // Confirm the tunnel immediately. Client-first protocols (including TLS) cannot
        // produce a response until the client receives this confirmation and starts relaying.
        // Server greetings and replies to initial_plaintext travel through the relay below.
        anyhow::Ok(upstream)
    };

    match upstream.await {
        Ok(mut upstream) => {
            tracing::debug!("Server: upstream connection established");

            http_protocol::Response {
                response: protocol::Response::Success {
                    initial_response: Vec::new(),
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
            let _ = cpxy_ng::relay::relay(&mut upstream, &mut conn, cpxy_ng::relay::DEFAULT_LINGER)
                .await;
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
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    trait TunnelStream: AsyncRead + AsyncWrite + Unpin {}
    impl<S: AsyncRead + AsyncWrite + Unpin> TunnelStream for S {}

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

    /// Opens a tunnel through the full HTTP/WebSocket/cipher stack. The deadline catches
    /// waiting for destination bytes before acknowledging a client-first connection.
    async fn open_tunnel(
        router: Router,
        server_host: &str,
        destination: SocketAddr,
        initial_plaintext: Vec<u8>,
    ) -> Box<dyn TunnelStream> {
        let cpxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cpxy_addr = cpxy_listener.local_addr().unwrap();
        let key: Key = derive_password("integration_test_key").into();
        let router = Arc::new(router);
        // Accept every connection: the outbound's warm pool connects alongside the request.
        tokio::spawn(async move {
            loop {
                let (conn, addr) = cpxy_listener.accept().await.unwrap();
                tokio::spawn(handle_connection(conn, addr, key, router.clone()));
            }
        });

        // ProtocolOutbound mirrors what client_cn does for each connection.
        let config = Config {
            host: server_host.to_string(),
            port: cpxy_addr.port(),
            key: derive_password("integration_test_key").into(),
        };
        let stream = timeout(
            Duration::from_millis(300),
            ProtocolOutbound::new(config).send(OutboundRequest {
                host: OutboundHost::Domain("127.0.0.1".to_string()),
                port: destination.port(),
                initial_plaintext,
            }),
        )
        .await
        .expect("tunnel setup waited for destination data")
        .expect("tunnel setup failed");
        Box::new(stream)
    }

    async fn assert_tunnel_echo(router: Router, server_host: &str, echo_addr: SocketAddr) {
        let mut stream = open_tunnel(router, server_host, echo_addr, vec![]).await;

        let msg = b"hello cpxy tunnel!";
        stream.write_all(msg).await.unwrap();
        let mut buf = vec![0u8; msg.len()];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, msg);
    }

    /// Older clients may still ask the server to open TLS to the target. That is refused with
    /// an error response rather than relaying their plaintext to the target.
    #[tokio::test]
    async fn rejects_legacy_tls_requests() {
        let echo_addr = spawn_echo_server().await;
        let key: Key = derive_password("integration_test_key").into();
        let (mut client, server) = tokio::io::duplex(4096);
        let server = tokio::spawn(handle_connection(
            server,
            "127.0.0.1:1".parse().unwrap(),
            key,
            Arc::new(Router::default()),
        ));

        let cipher = cpxy_ng::encrypt_stream::Configuration::random_full();
        http_protocol::Request {
            request: protocol::Request {
                host: "127.0.0.1".to_string(),
                port: echo_addr.port(),
                tls: true,
                client_send_cipher: cipher.clone(),
                server_send_cipher: cipher,
                initial_plaintext: vec![],
                timestamp_epoch_seconds: now_epoch_seconds(),
            },
            websocket_key: vec![0; 12],
            host: "127.0.0.1".to_string(),
        }
        .send_over_http(&mut client, &key)
        .await
        .unwrap();

        let (response, _) = http_protocol::Response::parse(client, &key)
            .await
            .map_err(|(e, _)| e)
            .unwrap()
            .take_head();
        assert!(
            matches!(&response.response, protocol::Response::Error { msg, .. } if msg.contains("TLS to the target is no longer supported")),
            "{:?}",
            response.response
        );
        server.await.unwrap().unwrap();
    }

    /// Spins up a real echo server, a real cpxy server, and a ProtocolOutbound client,
    /// then verifies data round-trips correctly through the full HTTP-upgrade →
    /// WebSocket-framing → ChaCha20-cipher stack.
    #[tokio::test]
    async fn full_tunnel_echo() {
        let echo_addr = spawn_echo_server().await;
        assert_tunnel_echo(Router::default(), "127.0.0.1", echo_addr).await;
    }

    #[tokio::test]
    async fn initial_payload_response_is_relayed() {
        let echo_addr = spawn_echo_server().await;
        let payload = b"initial request";
        let mut stream =
            open_tunnel(Router::default(), "127.0.0.1", echo_addr, payload.to_vec()).await;
        let mut reply = vec![0; payload.len()];
        timeout(Duration::from_secs(5), stream.read_exact(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply, payload);
        stream.write_all(b"next").await.unwrap();
        let mut reply = [0; 4];
        timeout(Duration::from_secs(5), stream.read_exact(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&reply, b"next");
    }

    #[tokio::test]
    async fn server_greeting_is_relayed_after_confirmation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (confirmed_tx, confirmed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // A delayed server greeting must not hold up tunnel confirmation.
            confirmed_rx.await.unwrap();
            stream.write_all(b"SSH-2.0-test\r\n").await.unwrap();
            let mut reply = [0; 5];
            stream.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"hello");
        });
        let mut stream = open_tunnel(Router::default(), "127.0.0.1", address, vec![]).await;
        confirmed_tx.send(()).unwrap();
        let mut greeting = [0; 14];
        timeout(Duration::from_secs(5), stream.read_exact(&mut greeting))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&greeting, b"SSH-2.0-test\r\n");
        stream.write_all(b"hello").await.unwrap();
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
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
            s.write_all(&[5, 0]).await.unwrap();

            let mut head = [0u8; 5];
            s.read_exact(&mut head).await.unwrap();
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

        assert_tunnel_echo(router, "localhost", echo_addr).await;
        assert_eq!(
            requested.await.unwrap(),
            ("127.0.0.1".to_string(), echo_addr.port())
        );
    }

    /// An echo server that serves any number of connections.
    async fn spawn_multi_echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut r, mut w) = tokio::io::split(stream);
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        addr
    }

    /// Runs a cpxy server behind a front that counts connections and, while `kill` is above
    /// zero, drops a connection as soon as its request arrives, like a NAT that forgot an idle
    /// connection.
    async fn spawn_cpxy_behind_front(
        kill: Arc<std::sync::atomic::AtomicUsize>,
        accepted: Arc<std::sync::atomic::AtomicUsize>,
    ) -> SocketAddr {
        use std::sync::atomic::Ordering;
        let cpxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cpxy_addr = cpxy.local_addr().unwrap();
        tokio::spawn(async move {
            let router = Arc::new(Router::default());
            loop {
                let (conn, addr) = cpxy.accept().await.unwrap();
                let key: Key = derive_password("integration_test_key").into();
                tokio::spawn(handle_connection(conn, addr, key, router.clone()));
            }
        });

        let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let front_addr = front.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut conn, _) = front.accept().await.unwrap();
                accepted.fetch_add(1, Ordering::SeqCst);
                let kill = kill.clone();
                tokio::spawn(async move {
                    let mut first = [0u8; 1];
                    if conn.read_exact(&mut first).await.is_err() {
                        return;
                    }
                    if kill
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok()
                    {
                        return;
                    }
                    let mut upstream = tokio::net::TcpStream::connect(cpxy_addr).await.unwrap();
                    upstream.write_all(&first).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
                });
            }
        });
        front_addr
    }

    async fn send_echo(outbound: &ProtocolOutbound, echo_addr: SocketAddr, msg: &[u8]) {
        let mut stream = timeout(
            Duration::from_secs(5),
            outbound.send(OutboundRequest {
                host: OutboundHost::Domain("127.0.0.1".to_string()),
                port: echo_addr.port(),
                initial_plaintext: msg.to_vec(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let mut reply = vec![0; msg.len()];
        timeout(Duration::from_secs(5), stream.read_exact(&mut reply))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply, msg);
    }

    async fn wait_for_accepted(accepted: &std::sync::atomic::AtomicUsize, n: usize) {
        timeout(Duration::from_secs(5), async {
            while accepted.load(std::sync::atomic::Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pool didn't pre-connect");
    }

    #[tokio::test]
    async fn requests_reuse_preconnected_sockets() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let echo_addr = spawn_multi_echo_server().await;
        let accepted = Arc::new(AtomicUsize::new(0));
        let front = spawn_cpxy_behind_front(Arc::new(AtomicUsize::new(0)), accepted.clone()).await;
        let outbound = ProtocolOutbound::new(
            format!("http://:integration_test_key@127.0.0.1:{}", front.port())
                .parse()
                .unwrap(),
        );

        // The first request connects fresh and starts warming two more.
        send_echo(&outbound, echo_addr, b"first").await;
        wait_for_accepted(&accepted, 3).await;

        // The second takes a warm socket, and the pool replaces it.
        send_echo(&outbound, echo_addr, b"second").await;
        wait_for_accepted(&accepted, 4).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn dead_preconnected_socket_retries_on_a_fresh_one() {
        use std::sync::atomic::AtomicUsize;
        let echo_addr = spawn_multi_echo_server().await;
        let accepted = Arc::new(AtomicUsize::new(0));
        let kill = Arc::new(AtomicUsize::new(0));
        let front = spawn_cpxy_behind_front(kill.clone(), accepted.clone()).await;
        let outbound = ProtocolOutbound::new(
            format!("http://:integration_test_key@127.0.0.1:{}", front.port())
                .parse()
                .unwrap(),
        );

        send_echo(&outbound, echo_addr, b"warm up").await;
        wait_for_accepted(&accepted, 3).await;

        // The warm socket looks open but dies once the request is sent.
        kill.store(1, std::sync::atomic::Ordering::SeqCst);
        send_echo(&outbound, echo_addr, b"after retry").await;
        assert_eq!(kill.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unmatched_host_header_connects_directly() {
        let echo_addr = spawn_echo_server().await;
        // Would fail tunnel setup if used: nothing listens on port 1.
        let router = Router::new(vec!["localhost=socks5://127.0.0.1:1".parse().unwrap()]);
        assert_tunnel_echo(router, "127.0.0.1", echo_addr).await;
    }
}
