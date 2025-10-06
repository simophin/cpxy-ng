use crate::handshaker::Handshaker;
use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use tokio::io::copy_bidirectional;
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

    let _ = copy_bidirectional(&mut conn, &mut upstream).await;
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
        let (stream, addr) = listener.accept().await?;
        tracing::info!(?addr, "Accepted connection");
        js.spawn(serve::<HS, _, _>(stream, outbound.clone()));
    }
}
