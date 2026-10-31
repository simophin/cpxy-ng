//! Plaintext SOCKS5 CONNECT with RFC 1928 no-authentication negotiation.
use anyhow::{Context, bail, ensure};
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use cpxy_ng::tls_stream::connect_tls;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone)]
pub struct Socks5Outbound(pub SocketAddr);

impl Outbound for Socks5Outbound {
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static> {
        let stream = tokio::time::timeout(Duration::from_secs(10), self.connect(&req))
            .await
            .context("SOCKS5 connect/handshake timed out")??;
        // `tls` describes the destination, never encryption of the SOCKS5 connection.
        let mut stream = connect_tls(req.host.host(), req.tls, stream).await?;
        stream.write_all(&req.initial_plaintext).await?;
        Ok(stream)
    }
}

impl Socks5Outbound {
    async fn connect(&self, req: &OutboundRequest) -> anyhow::Result<TcpStream> {
        let mut stream = TcpStream::connect(self.0)
            .await
            .context("Cannot connect to SOCKS5 proxy")?;
        stream.set_nodelay(true)?;
        stream.write_all(&[5, 1, 0]).await?;
        let mut method = [0; 2];
        stream.read_exact(&mut method).await?;
        ensure!(
            method == [5, 0],
            "SOCKS5 proxy refused no-authentication method: {method:?}"
        );
        let ip = match &req.host {
            OutboundHost::Resolved { ip: Some(ip), .. } => Some(*ip),
            _ => req.host.host().parse::<IpAddr>().ok(),
        };
        let mut request = vec![5, 1, 0];
        match ip {
            Some(IpAddr::V4(ip)) => {
                request.push(1);
                request.extend(ip.octets());
            }
            Some(IpAddr::V6(ip)) => {
                request.push(4);
                request.extend(ip.octets());
            }
            None => {
                let host = req.host.host().as_bytes();
                ensure!(
                    !host.is_empty() && host.len() <= 255,
                    "Invalid SOCKS5 destination hostname length"
                );
                request.extend([3, host.len() as u8]);
                request.extend(host);
            }
        }
        request.extend(req.port.to_be_bytes());
        stream.write_all(&request).await?;
        let mut head = [0; 4];
        stream.read_exact(&mut head).await?;
        ensure!(head[0] == 5 && head[2] == 0, "Invalid SOCKS5 reply header");
        ensure!(
            head[1] == 0,
            "SOCKS5 CONNECT failed with reply code {}",
            head[1]
        );
        let length = match head[3] {
            1 => 4,
            4 => 16,
            3 => stream.read_u8().await? as usize,
            other => bail!("Invalid SOCKS5 reply address type {other}"),
        };
        let mut bound = vec![0; length + 2];
        stream.read_exact(&mut bound).await?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn preserves_destinations_and_consumes_variable_reply_before_payload() {
        for (host, expected) in [
            (
                OutboundHost::Resolved {
                    domain: "ignored.example".into(),
                    ip: Some("1.2.3.4".parse().unwrap()),
                },
                vec![1, 1, 2, 3, 4],
            ),
            (OutboundHost::Domain("::1".into()), {
                let mut v = vec![4];
                v.extend(std::net::Ipv6Addr::LOCALHOST.octets());
                v
            }),
            (OutboundHost::Domain("example.com".into()), {
                let mut v = vec![3, 11];
                v.extend(b"example.com");
                v
            }),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = Socks5Outbound(listener.local_addr().unwrap());
            let fixture = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut greeting = [0; 3];
                socket.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 0]);
                socket.write_all(&[5, 0]).await.unwrap();
                let mut request = vec![0; 3 + expected.len() + 2];
                socket.read_exact(&mut request).await.unwrap();
                assert_eq!(&request[..3], &[5, 1, 0]);
                assert_eq!(&request[3..3 + expected.len()], expected);
                assert_eq!(&request[3 + expected.len()..], &443u16.to_be_bytes());
                // A domain-form BND.ADDR must be consumed, including its port.
                socket
                    .write_all(&[5, 0, 0, 3, 1, b'x', 0, 1])
                    .await
                    .unwrap();
                let mut payload = [0; 4];
                socket.read_exact(&mut payload).await.unwrap();
                assert_eq!(&payload, b"ping");
                socket.write_all(b"pong").await.unwrap();
            });
            let mut stream = proxy
                .send(OutboundRequest {
                    host,
                    port: 443,
                    tls: false,
                    initial_plaintext: b"ping".to_vec(),
                })
                .await
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, b"pong");
            fixture.await.unwrap();
        }
    }

    #[tokio::test]
    async fn refuses_authentication_and_failed_connect() {
        for (method, reply, expected) in [
            ([5, 2], vec![], "no-authentication"),
            ([5, 0], vec![5, 5, 0, 1], "reply code 5"),
            ([5, 0], vec![4, 0, 0, 1], "reply header"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = Socks5Outbound(listener.local_addr().unwrap());
            let fixture = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut greeting = [0; 3];
                socket.read_exact(&mut greeting).await.unwrap();
                socket.write_all(&method).await.unwrap();
                if !reply.is_empty() {
                    let mut request = [0; 10];
                    socket.read_exact(&mut request).await.unwrap();
                    socket.write_all(&reply).await.unwrap();
                }
            });
            let error = proxy
                .send(OutboundRequest {
                    host: OutboundHost::Domain("1.2.3.4".into()),
                    port: 80,
                    tls: false,
                    initial_plaintext: vec![],
                })
                .await
                .err()
                .unwrap();
            assert!(error.to_string().contains(expected), "{error}");
            fixture.await.unwrap();
        }
    }
}
