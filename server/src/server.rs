use cpxy_ng::encrypt_stream::CipherStream;
use cpxy_ng::time_util::now_epoch_seconds;
use cpxy_ng::{http_protocol, protocol, Key};
use anyhow::Context;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tracing::instrument;

trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}

pub fn configure_tls_connector() -> TlsConnector {
    let mut root_cert_store = RootCertStore::empty();
    root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    TlsConnector::from(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(),
    ))
}

#[instrument(ret, skip(conn, key, connector), level = "info")]
pub async fn handle_connection(
    conn: impl AsyncRead + AsyncWrite + Unpin,
    _from_addr: SocketAddr,
    key: Key,
    connector: TlsConnector,
) -> anyhow::Result<()> {
    let (req, mut conn) = http_protocol::parse_request(conn, &key)
        .await
        .context("Error reading request")?
        .take_head();

    let upstream = async {
        let upstream = TcpStream::connect((req.request.host.as_str(), req.request.port))
            .await
            .context("Error connecting to upstream")?;

        let mut upstream: Box<dyn Stream> = if req.request.tls {
            Box::new(
                connector
                    .connect(
                        req.host
                            .try_into()
                            .context("Unable to convert host to server name")?,
                        upstream,
                    )
                    .await
                    .context("TLS connection failed")?,
            )
        } else {
            Box::new(upstream)
        };

        tracing::debug!(
            "Writing initial plaintext: {}",
            std::str::from_utf8(&req.request.initial_plaintext).unwrap_or("<non-utf8>")
        );

        upstream
            .write_all(&req.request.initial_plaintext)
            .await
            .context("Error writing initial plaintext")?;

        anyhow::Ok(upstream)
    };

    match upstream.await {
        Ok(mut upstream) => {
            tracing::debug!("Upstream connection established");
            // Try to read some initial data if sent
            let mut initial_response = vec![0u8; 4096];
            let mut read_buf = ReadBuf::new(initial_response.as_mut_slice());
            match Pin::new(upstream.as_mut()).poll_read(
                &mut std::task::Context::from_waker(&mut futures::task::noop_waker()),
                &mut read_buf,
            ) {
                Poll::Ready(Ok(())) => {
                    let n = read_buf.filled().len();
                    initial_response.truncate(n);
                    tracing::debug!("Read {} bytes of initial data from upstream", n);
                }

                _ => initial_response.clear(),
            }

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
