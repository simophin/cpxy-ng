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
    fn send(
        &self,
        req: OutboundRequest,
    ) -> impl Future<
        Output = anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static>,
    > + Send
    + Sync;
}

impl<O: Outbound> Outbound for Arc<O> {
    fn send(
        &self,
        req: OutboundRequest,
    ) -> impl Future<
        Output = anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static>,
    > + Send
    + Sync {
        self.as_ref().send(req)
    }
}
