use crate::handshaker::Handshaker;
use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use cpxy_ng::relay::{DEFAULT_LINGER, relay};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

pub async fn serve<HS, S, OB>(stream: S, outbound: OB) -> anyhow::Result<()>
where
    HS: Handshaker<S>,
    <HS as Handshaker<S>>::RequestType: Into<OutboundRequest>,
    <HS as Handshaker<S>>::StreamType: AsyncRead + AsyncWrite + Unpin,
    OB: Outbound,
{
    let (req, handshake) = HS::accept(stream).await?;

    let req: OutboundRequest = req.into();
    let mut conn: <HS as Handshaker<S>>::StreamType;
    let mut upstream;

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

    let _ = relay(&mut conn, &mut upstream, DEFAULT_LINGER).await;
    Ok(())
}

pub async fn serve_listener<HS, OB>(listener: TcpListener, outbound: OB) -> anyhow::Result<()>
where
    HS: Handshaker<TcpStream> + Send + 'static,
    <HS as Handshaker<TcpStream>>::RequestType: Into<OutboundRequest>,
    <HS as Handshaker<TcpStream>>::StreamType: AsyncRead + AsyncWrite + Unpin + Send,
    OB: Outbound + Clone + Send + 'static,
{
    let mut js = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, addr) = accepted?;
                tracing::info!(?addr, "Accepted connection");
                js.spawn(serve::<HS, _, _>(stream, outbound.clone()));
            }
            // A JoinSet keeps every finished task until it is joined: reap them, or each
            // connection ever served stays allocated.
            Some(result) = js.join_next() => {
                if !matches!(result, Ok(Ok(()))) {
                    tracing::debug!(?result, "Proxy connection ended");
                }
            }
        }
    }
}
