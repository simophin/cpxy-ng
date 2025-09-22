use anyhow::{Context, bail, format_err};
use cpxy_ng::encrypt_stream::{CipherStream, Configuration};
use cpxy_ng::http_proxy::{
    ProxyRequest, ProxyRequestHttp, ProxyRequestSocket, parse_http_proxy_stream,
};
use cpxy_ng::key_util::random_vec;
use cpxy_ng::socks_stream::SocksStream;
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::tls_stream::TlsClientStream;
use cpxy_ng::{Key, http_protocol, protocol, socks_stream};
use std::fmt::{Debug, Formatter};
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::instrument;

#[derive(Default)]
pub struct Statistic {
    pub bytes_sent: Arc<AtomicUsize>,
    pub bytes_received: Arc<AtomicUsize>,
    pub last_delays: RwLock<Vec<Duration>>,
}

#[derive(Clone)]
pub struct UpstreamConfiguration {
    pub host: String,
    pub port: u16,
    pub key: Key,
    pub tls: bool,
}

impl Debug for UpstreamConfiguration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamConfiguration")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .finish()
    }
}

#[instrument(ret, skip(proxy_conn), fields(upstream_config = ?upstream_config.as_ref()))]
pub async fn accept_http_proxy_connection(
    proxy_conn: impl AsyncRead + AsyncWrite + Unpin,
    upstream_config: impl AsRef<UpstreamConfiguration>,
) -> anyhow::Result<()> {
    let (req, proxy_conn) = parse_http_proxy_stream(proxy_conn)
        .await
        .map_err(|(e, _)| e)
        .context("Error parsing HTTP proxy request")?
        .take_head();

    tracing::debug!(?req, "Successfully parsed request");

    match req {
        ProxyRequest::Http(req) => {
            handle_http_proxy_request(upstream_config.as_ref(), proxy_conn, req).await
        }

        ProxyRequest::Socket(req) => {
            handle_socket_proxy_request(upstream_config.as_ref(), proxy_conn, req).await
        }
    }
}

pub async fn accept_socks_proxy_connection(
    proxy_conn: impl AsyncBufRead + AsyncWrite + Unpin,
    upstream: impl AsRef<UpstreamConfiguration>,
) -> anyhow::Result<()> {
    let (mut proxy_conn, request) = SocksStream::accept(proxy_conn).await.map_err(|(e, _)| e)?;
    tracing::info!(?request, "About to serve socks5 proxy request");

    let request = match request {
        socks_stream::ProxyRequest::WithDomain(host, port) => ProxyRequestSocket { host, port },
        socks_stream::ProxyRequest::WithIP(addr) => ProxyRequestSocket {
            host: addr.ip().to_string(),
            port: addr.port(),
        },
    };

    let (mut stream, response) =
        send_socket_proxy_request(upstream.as_ref(), &mut proxy_conn, request)
            .await
            .map_err(|e| {
                tracing::error!(error=?e, "Error communicating with upstream server");
                e
            })?;

    match response.response {
        protocol::Response::Success {
            initial_response, ..
        } => {
            proxy_conn
                .write_all(&initial_response)
                .await
                .context("Error writing success response to client")?;

            let _ = tokio::io::copy_bidirectional(&mut proxy_conn, &mut stream).await;
            Ok(())
        }

        protocol::Response::Error { msg, .. } => {
            bail!("Error from upstream: {msg}");
        }
    }
}

async fn send_upstream_request(
    req: http_protocol::Request,
    upstream: &UpstreamConfiguration,
) -> anyhow::Result<(
    impl AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
    http_protocol::Response,
)> {
    let conn = TcpStream::connect((upstream.host.as_str(), upstream.port))
        .await
        .with_context(|| format!("Error connecting to upstream server: {upstream:?}"))?;

    let mut conn = if upstream.tls {
        TlsClientStream::connect_tls(upstream.host.as_str(), conn)
            .await
            .context("Error establishing TLS connection to upstream server")?
    } else {
        TlsClientStream::Plain(conn)
    };

    req.send_over_http(&mut conn, &upstream.key)
        .await
        .context("Error sending request to upstream server")?;

    let (resp, conn) = http_protocol::Response::parse(conn, &upstream.key)
        .await
        .map_err(|(e, _)| e)?
        .take_head();

    Ok((
        CipherStream::new(
            conn,
            &req.request.client_send_cipher,
            &req.request.server_send_cipher,
        ),
        resp,
    ))
}

async fn send_socket_proxy_request(
    upstream: &UpstreamConfiguration,
    proxy_conn: &mut (impl AsyncRead + AsyncWrite + Unpin),
    ProxyRequestSocket { host, port }: ProxyRequestSocket,
) -> anyhow::Result<(
    impl AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
    http_protocol::Response,
)> {
    let (client_send_cipher, server_send_cipher) = match port {
        443 | 465 | 993 | 5223 => (
            Configuration::random_partial(NonZeroUsize::new(32).unwrap()),
            Configuration::random_partial(NonZeroUsize::new(512).unwrap()),
        ),
        _ => (Configuration::random_full(), Configuration::random_full()),
    };

    let mut initial_plaintext = vec![0u8; 256];

    match timeout(
        Duration::from_millis(200),
        proxy_conn.read(&mut initial_plaintext),
    )
    .await
    {
        Ok(Ok(n)) => initial_plaintext.truncate(n),
        Err(_) => initial_plaintext.clear(), // Timeout, no initial data
        Ok(Err(e)) => return Err(e).context("Reading initial plaintext from client"),
    }

    let request = http_protocol::Request {
        request: protocol::Request {
            host,
            port,
            tls: false,
            client_send_cipher,
            server_send_cipher,
            initial_plaintext,
            timestamp_epoch_seconds: now_epoch_seconds(),
        },
        websocket_key: random_vec(16),
        host: upstream.host.clone(),
    };

    send_upstream_request(request, upstream).await
}

#[instrument(ret, skip(proxy_conn))]
async fn handle_socket_proxy_request(
    upstream: &UpstreamConfiguration,
    mut proxy_conn: impl AsyncRead + AsyncWrite + Unpin,
    req: ProxyRequestSocket,
) -> anyhow::Result<()> {
    match send_socket_proxy_request(upstream, &mut proxy_conn, req).await {
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

#[instrument(ret, skip(payload, proxy_conn))]
async fn handle_http_proxy_request(
    upstream: &UpstreamConfiguration,
    mut proxy_conn: impl AsyncRead + AsyncWrite + Unpin,
    ProxyRequestHttp {
        host,
        port,
        tls,
        payload,
    }: ProxyRequestHttp,
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
        websocket_key: random_vec(16),
        host: upstream.host.to_string(),
    };

    match send_upstream_request(request, upstream).await {
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
