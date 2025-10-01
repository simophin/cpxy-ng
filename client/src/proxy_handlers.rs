use crate::handshaker::Handshaker;
use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::copy_bidirectional;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;

pub async fn serve<HS, S, OB>(stream: S, outbound: OB) -> anyhow::Result<()>
where
    HS: Handshaker<S>,
    <HS as Handshaker<S>>::RequestType: Into<OutboundRequest>,
    <HS as Handshaker<S>>::StreamType: AsyncRead + AsyncWrite + Unpin,
    OB: Outbound,
{
    let (req, handshake) = HS::accept(stream).await?;

    let read_initial_data = HS::can_read_initial_data(&req);
    let mut req: OutboundRequest = req.into();
    let mut conn: <HS as Handshaker<S>>::StreamType;
    let mut upstream;

    if read_initial_data {
        conn = handshake
            .respond_ok()
            .await
            .context("Error responding ok")?;

        let mut buf = vec![0u8; 4096];
        let n = timeout(Duration::from_millis(200), conn.read(&mut buf))
            .await
            .unwrap_or(Ok(0))
            .context("Error reading initial data")?;

        tracing::debug!("Read {n} bytes of initial data from client");
        req.initial_plaintext.extend_from_slice(&buf[..n]);

        upstream = timeout(Duration::from_secs(50), outbound.send(req))
            .await
            .context("Timeout connecting to upstream server")??;
    } else {
        match outbound.send(req).await {
            Ok(up) => {
                upstream = up;
                conn = handshake
                    .respond_ok()
                    .await
                    .context("Error responding ok")?;
            }

            Err(e) => {
                handshake
                    .respond_err(&format!("Error sending upstream: {e}"))
                    .await
                    .context("Error responding err")?;
                return Err(e);
            }
        }
    }

    let _ = copy_bidirectional(&mut conn, &mut upstream).await;
    Ok(())
}
