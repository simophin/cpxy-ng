use crate::either_stream::EitherStream;
use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use std::net::{IpAddr, Ipv4Addr};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::lookup_host;

pub struct IPDivertOutbound<O1, O2, F> {
    pub outbound_a: Option<O1>,
    pub outbound_b: O2,
    pub should_use_a: F,
}

impl<O1, O2, F> Outbound for IPDivertOutbound<O1, O2, F>
where
    O1: Outbound,
    O2: Outbound,
    F: Fn(Ipv4Addr) -> bool,
{
    async fn send(
        &self,
        mut req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin> {
        if let Some(outbound_a) = self.outbound_a.as_ref() {
            let domain = req.host.as_str();
            let ip = lookup_host(format!("{domain}:80"))
                .await
                .with_context(|| format!("Error resolving hostname: {domain}"))?
                .next()
                .with_context(|| format!("No addresses found for domain {domain}"))?
                .ip();

            match ip {
                IpAddr::V4(ip) if (self.should_use_a)(ip) => {
                    req.host = ip.to_string();
                    outbound_a.send(req).await.map(EitherStream::Left)
                }

                _ => self.outbound_b.send(req).await.map(EitherStream::Right),
            }
        } else {
            self.outbound_b.send(req).await.map(EitherStream::Right)
        }
    }
}
