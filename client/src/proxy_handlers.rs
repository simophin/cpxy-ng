use crate::handshaker::Handshaker;
use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use futures::future::{Either, select, select_all};
use std::pin::{Pin, pin};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::copy_bidirectional;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{Instrument, info_span};

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

pub async fn serve_listener<HS, OB>(listener: TcpListener, outbound: OB) -> anyhow::Result<()>
where
    HS: Handshaker<TcpStream>,
    <HS as Handshaker<TcpStream>>::RequestType: Into<OutboundRequest>,
    <HS as Handshaker<TcpStream>>::StreamType: AsyncRead + AsyncWrite + Unpin,
    OB: Outbound + Clone,
{
    let mut serve_futs =
        select_all(Vec::<Pin<Box<dyn Future<Output = anyhow::Result<()>>>>>::new());

    loop {
        match select(pin!(listener.accept()), serve_futs).await {
            Either::Left((result, mut serving)) => {
                let (stream, addr) = result.context("Error accepting incoming connection")?;
                let outbound = outbound.clone();
                tracing::info!(?addr, "Accepted connection");

                let mut futs = serving.into_inner();
                futs.push(Box::pin(
                    serve::<HS, _, _>(stream, outbound.clone())
                        .instrument(info_span!("serve_proxy_client", ?addr)),
                ));
                serve_futs = select_all(futs);
            }

            Either::Right(((res, _, remaining), _)) => {
                serve_futs = select_all(remaining);
                match res {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!(?e, "Error in task");
                    }
                }
            }
        }
    }
}
