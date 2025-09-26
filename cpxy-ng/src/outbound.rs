use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Clone)]
pub struct OutboundRequest {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub initial_plaintext: Vec<u8>,
}

impl Debug for OutboundRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundRequest")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field(
                "initial_plaintext",
                &format!("<{} bytes>", self.initial_plaintext.len()),
            )
            .finish()
    }
}

pub trait Outbound {
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin>;
}

impl<O: Outbound> Outbound for &O {
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin> {
        (*self).send(req).await
    }
}

impl<O: Outbound> Outbound for Arc<O> {
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin> {
        self.as_ref().send(req).await
    }
}
