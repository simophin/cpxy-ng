//! A transparent TCP proxy: serves connections the kernel redirected to its listener (an
//! nftables `redirect`), to the destination each was originally sent to.
//!
//! There is no protocol to answer: the original destination comes from conntrack
//! (`SO_ORIGINAL_DST`), and the accepted stream is relayed as is. A TPROXY mode would read the
//! destination with `getsockname()` instead, on a listener with `IP_TRANSPARENT`.
use crate::handshaker::Handshaker;
use crate::socks_proxy_server::ProxyRequest;
use anyhow::{Context, bail};
use std::net::SocketAddr;
use tokio::net::TcpStream;

pub struct TransparentHandshaker {
    stream: TcpStream,
}

/// The destination a redirected connection was sent to. `original` is what conntrack recorded,
/// if anything; `local` is the address the connection reached.
///
/// Refuses a connection that was not redirected: without a NAT record, or when the original
/// destination is the listener itself (a client connected to its port directly). Proxying
/// those would connect back to this listener.
pub fn redirected_destination(
    original: Option<SocketAddr>,
    local: SocketAddr,
) -> anyhow::Result<SocketAddr> {
    let canonical = |addr: SocketAddr| SocketAddr::new(addr.ip().to_canonical(), addr.port());
    match original {
        Some(original) if canonical(original) != canonical(local) => Ok(original),
        _ => bail!("Connection to {local} was not redirected"),
    }
}

/// The original destination conntrack holds for `stream`, or None when it has no NAT record.
fn original_destination(stream: &TcpStream, local: SocketAddr) -> Option<SocketAddr> {
    let socket = socket2::SockRef::from(stream);
    let original = match local {
        SocketAddr::V4(_) => socket.original_dst_v4(),
        SocketAddr::V6(_) => socket.original_dst_v6(),
    };
    match original {
        Ok(addr) => addr.as_socket(),
        Err(error) => {
            tracing::debug!(%local, %error, "No original destination");
            None
        }
    }
}

impl Handshaker<TcpStream> for TransparentHandshaker {
    type StreamType = TcpStream;
    type RequestType = ProxyRequest;

    async fn accept(stream: TcpStream) -> anyhow::Result<(ProxyRequest, Self)> {
        let local = stream.local_addr().context("Reading the local address")?;
        let destination = redirected_destination(original_destination(&stream, local), local)?;
        Ok((ProxyRequest::WithIP(destination), Self { stream }))
    }

    async fn respond_ok(self) -> anyhow::Result<TcpStream> {
        Ok(self.stream)
    }

    /// Closes the connection: the client only learns that it failed.
    async fn respond_err(self, _msg: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn stream_mut(&mut self) -> &mut TcpStream {
        &mut self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpxy_ng::outbound::{OutboundHost, OutboundRequest};
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn refuses_connections_that_were_not_redirected() {
        let local: SocketAddr = "192.168.8.1:5100".parse().unwrap();
        assert!(redirected_destination(None, local).is_err());
        assert!(redirected_destination(Some(local), local).is_err());
        // The same listener seen through an IPv4-mapped IPv6 address
        let mapped: SocketAddr = "[::ffff:192.168.8.1]:5100".parse().unwrap();
        assert!(redirected_destination(Some(mapped), local).is_err());
        assert!(redirected_destination(Some(local), mapped).is_err());
    }

    #[test]
    fn redirected_destination_is_the_original_one() {
        let local: SocketAddr = "192.168.8.1:5100".parse().unwrap();
        for original in ["93.184.216.34:443", "192.168.8.1:80", "10.0.0.1:5100"] {
            let original: SocketAddr = original.parse().unwrap();
            assert_eq!(
                redirected_destination(Some(original), local).unwrap(),
                original
            );
        }
    }

    #[test]
    fn request_connects_to_the_ip_without_a_hostname() {
        let original: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let request: OutboundRequest = ProxyRequest::WithIP(original).into();
        assert_eq!(request.port, 443);
        assert!(!request.tls);
        assert!(request.initial_plaintext.is_empty());
        match request.host {
            OutboundHost::Resolved { ip, .. } => assert_eq!(ip, Some(original.ip())),
            _ => panic!("not connected by IP"),
        }
    }

    #[tokio::test]
    async fn a_direct_connection_to_the_listener_is_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        assert!(TransparentHandshaker::accept(accepted).await.is_err());
        // The handshaker dropped the stream: the client sees the connection end.
        let mut buf = [0; 1];
        let read = client.read(&mut buf).await;
        assert!(matches!(read, Ok(0)) || read.is_err());
    }
}
