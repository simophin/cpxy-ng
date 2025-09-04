use crate::encrypt_stream::{CipherStream, Configuration};
use crate::http_proxy::{
    ProxyRequest, ProxyRequestHttp, ProxyRequestSocket, parse_http_proxy_stream,
};
use crate::time_util::now_epoch_seconds;
use crate::{http_protocol, protocol};
use anyhow::{Context, format_err};
use chacha20poly1305::Key;
use std::num::NonZeroUsize;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::instrument;

#[instrument(ret, skip(key, proxy_conn))]
pub async fn accept_proxy_connection(
    proxy_conn: impl AsyncRead + AsyncWrite + Unpin,
    server_host: String,
    server_port: u16,
    key: Key,
) -> anyhow::Result<()> {
    let (req, proxy_conn) = parse_http_proxy_stream(proxy_conn)
        .await
        .context("Error parsing HTTP proxy request")?
        .take_head();

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

    let (resp, conn) = http_protocol::parse_response(conn, key).await?.take_head();

    Ok((
        CipherStream::new(
            conn,
            &req.request.client_send_cipher,
            &req.request.server_send_cipher,
        ),
        resp,
    ))
}

#[instrument(ret, skip(key, proxy_conn))]
async fn handle_socket_proxy_request(
    upstream_host: &str,
    upstream_port: u16,
    mut proxy_conn: impl AsyncRead + AsyncWrite + Unpin,
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
    mut proxy_conn: impl AsyncRead + AsyncWrite + Unpin,
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
                    if !initial_response.is_empty() {
                        proxy_conn
                            .write_all(&initial_response)
                            .await
                            .context("Writing initial response")?;
                    }
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

            let _ = tokio::io::copy_bidirectional(&mut proxy_conn, &mut upstream_conn).await;
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
