use anyhow::Context;
use cpxy_ng::either_stream::EitherStream;
use cpxy_ng::outbound::{Outbound, OutboundHost, OutboundRequest};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::lookup_host;

pub struct IPDivertOutbound<O1, O2, F> {
    pub outbound_a: Option<O1>,
    pub outbound_b: O2,
    pub should_use_a: F,
}

impl<O1, O2, F> Outbound for IPDivertOutbound<O1, O2, F>
where
    O1: Outbound + Sync,
    O2: Outbound + Sync,
    F: Fn(anyhow::Result<Option<Ipv4Addr>>) -> bool + Sync,
{
    async fn send(
        &self,
        mut req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> {
        if let Some(outbound_a) = self.outbound_a.as_ref() {
            let ip_to_test: anyhow::Result<Option<Ipv4Addr>>;
            // Resolve the domain first
            req.host = match req.host {
                OutboundHost::Domain(domain) => match resolve_domain(domain.as_str()).await {
                    Ok(Some(ip)) => {
                        ip_to_test = Ok(Some(ip));

                        OutboundHost::Resolved {
                            domain,
                            ip: IpAddr::V4(ip),
                        }
                    }

                    Ok(None) => {
                        ip_to_test = Ok(None);
                        OutboundHost::Domain(domain)
                    }

                    Err(e) => {
                        ip_to_test = Err(e);
                        OutboundHost::Domain(domain)
                    }
                },

                OutboundHost::Resolved {
                    ip: IpAddr::V4(ip),
                    domain,
                } => {
                    ip_to_test = Ok(Some(ip));
                    OutboundHost::Resolved {
                        domain,
                        ip: IpAddr::V4(ip),
                    }
                }

                v => {
                    ip_to_test = Ok(None);
                    v
                }
            };

            if (self.should_use_a)(ip_to_test) {
                return outbound_a.send(req).await.map(EitherStream::Left);
            }
        }

        self.outbound_b.send(req).await.map(EitherStream::Right)
    }
}

async fn resolve_domain(domain: &str) -> anyhow::Result<Option<Ipv4Addr>> {
    let mut addrs = lookup_host((domain, 80))
        .await
        .with_context(|| format!("Error resolving hostname: {domain}"))?;
    Ok(addrs.find_map(|addr| match addr {
        SocketAddr::V4(v4) => Some(*v4.ip()),
        _ => None,
    }))
}
