//! Answers the DNS of clients that use this node as their exit node.
//!
//! Those clients send DNS as DNS over HTTPS to the exit node's peer API (/dns-query), never as
//! packets, so the packet engine does not see it. Tailscale's peer API answers the tailnet's own
//! names itself and forwards the rest to the exit node's host resolver: this server, on a loopback
//! port, which answers with the engine's split DNS. It speaks plain DNS over UDP and, for answers
//! too large for UDP, over TCP on the same port.

use mobile_engine::EngineHandle;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// The largest DNS message.
const MAX_MESSAGE: usize = 65535;

/// Binds UDP and TCP on one loopback port and serves both. Returns the address.
pub async fn serve(engine: Arc<EngineHandle>) -> anyhow::Result<SocketAddr> {
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let addr = udp.local_addr()?;
    let tcp = TcpListener::bind(addr).await?;
    tokio::spawn(serve_udp(Arc::new(udp), engine.clone()));
    tokio::spawn(serve_tcp(tcp, engine));
    Ok(addr)
}

/// Answers `query`, or `None` if the engine cannot. Runs on a blocking thread: the engine
/// answers on its own runtime and blocks the caller meanwhile.
async fn resolve(engine: &Arc<EngineHandle>, query: Vec<u8>) -> Option<Vec<u8>> {
    tracing::debug!(
        "exitdns: {}",
        question(&query).unwrap_or_else(|| "?".to_owned())
    );
    let engine = engine.clone();
    match tokio::task::spawn_blocking(move || engine.resolve_dns(&query)).await {
        Ok(Ok(answer)) => Some(answer),
        Ok(Err(e)) => {
            tracing::debug!("Exit node DNS: {e:#}");
            None
        }
        Err(_) => None,
    }
}

/// The name asked for in a wire-format query, for the log.
fn question(query: &[u8]) -> Option<String> {
    let mut name = String::new();
    // The question follows the 12-byte header, as length-prefixed labels.
    let mut at = 12;
    loop {
        let len = usize::from(*query.get(at)?);
        if len == 0 {
            break;
        }
        // A pointer is never the first name in a query.
        let label = query.get(at + 1..at + 1 + len).filter(|_| len < 64)?;
        name.push_str(&String::from_utf8_lossy(label));
        name.push('.');
        at += 1 + len;
    }
    Some(if name.is_empty() {
        ".".to_owned()
    } else {
        name
    })
}

async fn serve_udp(socket: Arc<UdpSocket>, engine: Arc<EngineHandle>) {
    let mut buf = vec![0u8; MAX_MESSAGE];
    loop {
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Exit node DNS over UDP: {e}");
                continue;
            }
        };
        let query = buf[..n].to_vec();
        let (socket, engine) = (socket.clone(), engine.clone());
        tokio::spawn(async move {
            if let Some(answer) = resolve(&engine, query).await {
                let _ = socket.send_to(&answer, peer).await;
            }
        });
    }
}

async fn serve_tcp(listener: TcpListener, engine: Arc<EngineHandle>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(serve_tcp_conn(stream, engine.clone()));
            }
            Err(e) => tracing::warn!("Exit node DNS over TCP: {e}"),
        }
    }
}

/// Answers length-prefixed queries until the client closes.
async fn serve_tcp_conn(mut stream: TcpStream, engine: Arc<EngineHandle>) -> std::io::Result<()> {
    loop {
        let len = stream.read_u16().await?;
        let mut query = vec![0u8; usize::from(len)];
        stream.read_exact(&mut query).await?;
        let Some(answer) = resolve(&engine, query).await else {
            return Ok(());
        };
        let Ok(len) = u16::try_from(answer.len()) else {
            return Ok(());
        };
        stream.write_u16(len).await?;
        stream.write_all(&answer).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::question;

    #[test]
    fn question_names_the_query() {
        let mut query = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        query.extend_from_slice(b"\x06ip-api\x03com\x00\x00\x01\x00\x01");
        assert_eq!(question(&query).as_deref(), Some("ip-api.com."));
        assert_eq!(question(&query[..15]), None);
        assert_eq!(question(&[0; 13]).as_deref(), Some("."));
    }
}
