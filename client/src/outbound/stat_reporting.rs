use crate::stats_server::OutboundEvent;
use anyhow::Context;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use std::borrow::Cow;
use std::time::{Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;

pub struct StatReportingOutbound<O> {
    pub name: Cow<'static, str>,
    pub inner: O,
    pub events_tx: broadcast::Sender<OutboundEvent>,
}

impl<O> Outbound for StatReportingOutbound<O>
where
    O: Outbound + Sync,
{
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static> {
        let start = Instant::now();
        let request_time_mills = SystemTime::UNIX_EPOCH
            .elapsed()
            .context("Error getting system time")?
            .as_millis() as u64;
        let host = req.host.host().to_string();
        let port = req.port;
        let r = self.inner.send(req).await;
        let delay_mills = start.elapsed().as_millis() as usize;

        let event = match &r {
            Ok(_) => OutboundEvent::Connected {
                host,
                port,
                outbound: self.name.clone(),
                delay_mills,
                request_time_mills,
            },

            Err(e) => OutboundEvent::Error {
                host,
                outbound: self.name.clone(),
                port,
                delay_mills,
                error: format!("{e:#}"),
                request_time_mills,
            },
        };

        let _ = self.events_tx.send(event);

        r
    }
}
