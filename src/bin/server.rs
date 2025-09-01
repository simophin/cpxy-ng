use anyhow::Context;
use chacha20::cipher::crypto_common::rand_core::OsRng;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit};
use cpxy_ng::{http_protocol, protocol};
use dotenvy::dotenv;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tracing::{Instrument, instrument};

#[tokio::main]
async fn main() {
    let _ = dotenv();
    tracing_subscriber::fmt::init();

    let listener = TcpListener::bind(("127.0.0.1", 9000))
        .await
        .expect("Error binding address");

    let mut root_cert_store = RootCertStore::empty();
    root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let connector = TlsConnector::from(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(),
    ));

    tracing::info!("Server listening on {}", listener.local_addr().unwrap());

    let key: Key = Key::from([0u8; 32]);

    loop {
        let (socket, addr) = listener.accept().await.expect("Error accepting connection");
        tokio::spawn(handle_connection(socket, addr, key, connector.clone()));
    }
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}

#[instrument(ret, skip(conn, key, connector), level = "info")]
async fn handle_connection(
    mut conn: TcpStream,
    addr: SocketAddr,
    key: Key,
    connector: TlsConnector,
) -> anyhow::Result<()> {
    let (req, extra_data) = http_protocol::Request::from_http_stream(&mut conn, &key)
        .await
        .context("Error reading request")?;

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

        upstream
            .write_all(&req.request.initial_plaintext)
            .await
            .context("Error writing initial plaintext")?;

        upstream
            .write_all(&extra_data)
            .await
            .context("Error writing initial data to upstream")?;
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
                    timestamp_epoch_seconds: SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                },
                websocket_key: req.websocket_key,
            }
            .send_over_http(&mut conn, &key)
            .await
            .context("Error sending response")?;

            tokio::io::copy_bidirectional(&mut upstream, &mut conn)
                .await
                .context("Error while transfering data")?;
            anyhow::Ok(())
        }

        Err(e) => http_protocol::Response {
            response: protocol::Response::Error {
                msg: format!("{e:?}"),
                timestamp_epoch_seconds: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            },
            websocket_key: req.websocket_key,
        }
        .send_over_http(&mut conn, &key)
        .await
        .context("Error sending response"),
    }
}
