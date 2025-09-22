use anyhow::{Context, bail, ensure};
use pin_project_lite::pin_project;
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pin_project! {
    pub struct SocksStream<S> {
        #[pin]
        inner: S,
    }
}

#[derive(Debug)]
pub enum ProxyRequest {
    WithDomain(String, u16),
    WithIP(SocketAddr),
}

impl<S> SocksStream<S> {
    pub async fn accept(mut stream: S) -> Result<(Self, ProxyRequest), (anyhow::Error, S)>
    where
        S: AsyncBufRead + AsyncWrite + Unpin,
    {
        let r = async {
            ensure!(
                stream.read_u8().await.context("Error reading version")? == 5,
                "Unsupported SOCKS version while waiting for initial greeting"
            );

            let mut methods =
                vec![0; stream.read_u8().await.context("Error reading n_methods")? as usize];
            stream
                .read_exact(&mut methods)
                .await
                .context("Error reading auth methods")?;

            ensure!(methods.contains(&0), "No supported authentication methods");

            // Reply with auth method selection
            stream
                .write_all(&[5, 0])
                .await
                .context("Error writing auth method selection")?;

            // Now read the actual request
            ensure!(
                stream.read_u8().await.context("Error reading version")? == 5,
                "Unsupported SOCKS version while waiting for request"
            );

            ensure!(
                stream.read_u8().await.context("Error reading command")? == 1,
                "Only CONNECT command is supported"
            );

            ensure!(
                stream
                    .read_u8()
                    .await
                    .context("Error reading reserved byte")?
                    == 0,
                "Invalid reserved byte"
            );

            // Read address type
            let addr_type = stream
                .read_u8()
                .await
                .context("Error reading address type")?;

            let dest = match addr_type {
                1 => {
                    // IPv4
                    let mut ip_bytes = [0; 4];
                    stream
                        .read_exact(&mut ip_bytes)
                        .await
                        .context("Error reading IPv4 address")?;
                    let ip = IpAddr::from(ip_bytes);
                    let port = stream.read_u16().await.context("Error reading port")?;
                    ProxyRequest::WithIP(SocketAddr::new(ip, port))
                }
                3 => {
                    // Domain name
                    let domain_len = stream
                        .read_u8()
                        .await
                        .context("Error reading domain length")?;
                    let mut domain_bytes = vec![0; domain_len as usize];
                    stream
                        .read_exact(&mut domain_bytes)
                        .await
                        .context("Error reading domain")?;
                    let domain =
                        String::from_utf8(domain_bytes).context("Invalid UTF-8 in domain")?;
                    let port = stream.read_u16().await.context("Error reading port")?;
                    ProxyRequest::WithDomain(domain, port)
                }
                4 => {
                    // IPv6
                    let mut ip_bytes = [0; 16];
                    stream
                        .read_exact(&mut ip_bytes)
                        .await
                        .context("Error reading IPv6 address")?;
                    let ip = IpAddr::from(ip_bytes);
                    let port = stream.read_u16().await.context("Error reading port")?;
                    ProxyRequest::WithIP(SocketAddr::new(ip, port))
                }
                _ => {
                    bail!("Unsupported address type {addr_type}");
                }
            };

            // Reply with success
            stream
                .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .context("Error writing success reply")?;

            anyhow::Ok(dest)
        };

        match r.await {
            Ok(req) => Ok((SocksStream { inner: stream }, req)),
            Err(e) => {
                tracing::error!(?e, "Error during SOCKS5 handshake");
                Err((e, stream))
            }
        }
    }
}

impl<S: AsyncRead> AsyncRead for SocksStream<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().inner.poll_read(cx, buf)
    }
}

impl<S: AsyncWrite> AsyncWrite for SocksStream<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().inner.poll_shutdown(cx)
    }
}
