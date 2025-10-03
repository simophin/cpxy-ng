use crate::handshaker::Handshaker;
use anyhow::{Context, bail, ensure};
use cpxy_ng::outbound::OutboundRequest;
use std::fmt::{Debug, Formatter};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tracing::instrument;

pub struct SocksProxyHandshaker<S> {
    stream: BufReader<S>,
}

impl<S> Debug for SocksProxyHandshaker<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("SocksProxyHandshaker")
    }
}

#[derive(Debug)]
pub enum ProxyRequest {
    WithDomain(String, u16),
    WithIP(SocketAddr),
}

impl From<ProxyRequest> for OutboundRequest {
    fn from(value: ProxyRequest) -> Self {
        match value {
            ProxyRequest::WithDomain(host, port) => Self {
                host,
                port,
                tls: false,
                initial_plaintext: vec![],
            },
            ProxyRequest::WithIP(addr) => Self {
                host: addr.ip().to_string(),
                port: addr.port(),
                tls: false,
                initial_plaintext: vec![],
            },
        }
    }
}

impl<S> Handshaker<S> for SocksProxyHandshaker<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    type StreamType = BufReader<S>;
    type RequestType = ProxyRequest;

    #[instrument(ret, skip(stream))]
    async fn accept(stream: S) -> anyhow::Result<(ProxyRequest, Self)> {
        let mut stream = BufReader::new(stream);
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
                let domain = String::from_utf8(domain_bytes).context("Invalid UTF-8 in domain")?;
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

        anyhow::Ok((dest, Self { stream }))
    }

    async fn respond_ok(mut self) -> anyhow::Result<Self::StreamType>
    where
        S: AsyncWrite + Unpin,
    {
        // Reply with success
        self.stream
            .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .context("Error writing success reply")?;

        Ok(self.stream)
    }

    async fn respond_err(self, _msg: &str) -> anyhow::Result<()>
    where
        S: AsyncWrite + Unpin,
    {
        Ok(())
    }

    fn stream_mut(&mut self) -> &mut Self::StreamType {
        &mut self.stream
    }
}
