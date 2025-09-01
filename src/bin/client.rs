use anyhow::{Context, format_err};
use chacha20poly1305::aead::OsRng;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit};
use cpxy_ng::encrypt_stream::{CipherStream, Configuration};
use cpxy_ng::http_proxy::{ProxyRequest, ProxyRequestHttp, ProxyRequestSocket};
use cpxy_ng::{http_protocol, protocol};
use dotenvy::dotenv;
use std::io::Cursor;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, join};
use tokio::net::{TcpListener, TcpStream};
use tracing::instrument;

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let listener = TcpListener::bind(("127.0.0.1", 8080))
        .await
        .expect("Error binding address");

    tracing::debug!("Started listening on {}", listener.local_addr().unwrap());

    let server = "127.0.0.1".to_string();
    let server_port = 9000;
    let key: Key = Key::from([0u8; 32]);

    loop {
        let (client, addr) = listener.accept().await.expect("Error accepting connection");
        tracing::info!("Accepted connection from {addr}");

        tokio::spawn(accept_connection(
            client,
            addr,
            server.clone(),
            server_port,
            key,
        ));
    }
}

#[instrument(ret, skip(key, proxy_conn))]
async fn accept_connection(
    mut proxy_conn: TcpStream,
    addr: SocketAddr,
    server_host: String,
    server_port: u16,
    key: Key,
) -> anyhow::Result<()> {
    let req = ProxyRequest::from_http_stream(&mut proxy_conn)
        .await
        .context("Error parsing HTTP proxy request")?;

    tracing::debug!(?req, "Successfully parsed request");

    match req {
        ProxyRequest::Http(req) => {
            handle_http_proxy_request(&server_host, server_port, proxy_conn, req, &key).await
        }

        ProxyRequest::Socket(req) => {
            handle_socket_proxy_request(&server_host, server_port, proxy_conn, req, &key).await
        }
    }
}

async fn send_upstream_request(
    req: http_protocol::Request,
    upstream_host: &str,
    upstream_port: u16,
    key: &Key,
) -> anyhow::Result<(
    impl AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
    http_protocol::Response,
)> {
    let mut conn = TcpStream::connect((upstream_host, upstream_port))
        .await
        .context("Error connecting to upstream server")?;

    req.send_over_http(&mut conn, key)
        .await
        .context("Error sending request to upstream server")?;

    let (resp, extra_bytes) = http_protocol::Response::from_http_stream(&mut conn, key).await?;
    let (read, write) = conn.into_split();

    Ok((
        CipherStream::new(
            join(AsyncReadExt::chain(Cursor::new(extra_bytes), read), write),
            &req.request.client_send_cipher,
            &req.request.server_send_cipher,
        ),
        resp,
    ))
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[instrument(ret, skip(key, proxy_conn))]
async fn handle_socket_proxy_request(
    upstream_host: &str,
    upstream_port: u16,
    mut proxy_conn: TcpStream,
    ProxyRequestSocket { host, port }: ProxyRequestSocket,
    key: &Key,
) -> anyhow::Result<()> {
    let (client_send_cipher, server_send_cipher) = match port {
        443 | 465 | 993 | 5223 => (
            Configuration::random_partial(NonZeroUsize::new(32).unwrap()),
            Configuration::Plaintext,
        ),
        _ => (Configuration::random_full(), Configuration::random_full()),
    };

    let request = http_protocol::Request {
        request: protocol::Request {
            host,
            port,
            tls: false,
            client_send_cipher,
            server_send_cipher,
            initial_plaintext: vec![],
            timestamp_epoch_seconds: now_epoch_seconds(),
        },
        websocket_key: None,
        host: upstream_host.to_string(),
    };

    match send_upstream_request(request, upstream_host, upstream_port, key).await {
        Ok((mut upstream_conn, resp)) => {
            match resp.response {
                protocol::Response::Success {
                    initial_response, ..
                } => {
                    proxy_conn
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .context("Writing 200 Connection Established to client")?;

                    proxy_conn
                        .write_all(&initial_response)
                        .await
                        .context("Writing initial response")?;
                }

                protocol::Response::Error { msg, .. } => {
                    let response = construct_error_http_response(500, &msg);
                    tracing::debug!(error = msg, "Upstream server returned error");

                    proxy_conn
                        .write_all(response.as_bytes())
                        .await
                        .context("Writing error response to client")?;
                    return Err(format_err!("Upstream server returned error: {msg}"));
                }
            }

            tokio::io::copy_bidirectional(&mut proxy_conn, &mut upstream_conn).await?;
            Ok(())
        }
        Err(e) => {
            tracing::error!(error=?e, "Error communicating with upstream server");
            let response = construct_error_http_response(500, &format!("{e:?}"));
            proxy_conn
                .write_all(response.as_bytes())
                .await
                .context("Writing error response to client")?;
            Err(e)
        }
    }
}

#[instrument(ret, skip(key, payload, proxy_conn))]
async fn handle_http_proxy_request(
    upstream_host: &str,
    upstream_port: u16,
    mut proxy_conn: TcpStream,
    ProxyRequestHttp {
        host,
        port,
        tls,
        payload,
    }: ProxyRequestHttp,
    key: &Key,
) -> anyhow::Result<()> {
    let client_send_cipher = Configuration::random_full();
    let server_send_cipher = Configuration::random_full();

    let request = http_protocol::Request {
        request: protocol::Request {
            host,
            port,
            tls,
            client_send_cipher,
            server_send_cipher,
            initial_plaintext: payload,
            timestamp_epoch_seconds: now_epoch_seconds(),
        },
        websocket_key: None,
        host: upstream_host.to_string(),
    };

    match send_upstream_request(request, upstream_host, upstream_port, key).await {
        Ok((mut upstream_conn, resp)) => {
            match resp.response {
                protocol::Response::Success {
                    initial_response, ..
                } => {
                    proxy_conn
                        .write_all(&initial_response)
                        .await
                        .context("Writing initial response")?;
                }

                protocol::Response::Error { msg, .. } => {
                    let response = construct_error_http_response(500, &msg);
                    tracing::debug!(error = msg, "Upstream server returned error");

                    proxy_conn
                        .write_all(response.as_bytes())
                        .await
                        .context("Writing error response to client")?;
                    return Err(format_err!("Upstream server returned error: {msg}"));
                }
            }

            tokio::io::copy_bidirectional(&mut proxy_conn, &mut upstream_conn).await?;
            Ok(())
        }
        Err(e) => {
            tracing::error!(error=?e, "Error communicating with upstream server");
            let response = construct_error_http_response(500, format!("{e:?}").as_str());
            proxy_conn
                .write_all(response.as_bytes())
                .await
                .context("Writing error response to client")?;
            Err(e)
        }
    }
}

fn construct_error_http_response(code: u16, msg: &str) -> String {
    format!(
        "HTTP/1.1 {code} Internal Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        msg.as_bytes().len(),
        msg
    )
}
