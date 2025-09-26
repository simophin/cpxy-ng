use crate::stats_outbound::StatsOutbound;
use anyhow::ensure;
use cpxy_ng::outbound::{Outbound, OutboundRequest};
use rand::prelude::IndexedRandom;
use rand::rng;
use std::cmp::min;
use std::fmt::Debug;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::instrument;

#[derive(Debug, Clone)]
pub struct SelectorOutbound<O> {
    outbounds: Vec<StatsOutbound<O>>,
}

impl<O: Debug> SelectorOutbound<O> {
    pub fn new(outbounds: impl Iterator<Item = O>) -> anyhow::Result<Self> {
        let s = Self {
            outbounds: outbounds.map(StatsOutbound::new).collect(),
        };

        ensure!(!s.outbounds.is_empty(), "No outbounds provided");
        Ok(s)
    }

    fn calculate_weight(delay_ms: u64, error_rate_percent: usize) -> i64 {
        // Convert delay to milliseconds
        let delay_ms = delay_ms as f64;
        let error_rate = error_rate_percent as f64;

        // Parameters to control decay aggressiveness
        let delay_decay = 0.0005; // higher = more aggressive penalty for delay
        let error_decay = 0.01; // higher = more aggressive penalty for error

        // Exponential decay for both factors
        let weight =
            100_000.0 * (-delay_decay * delay_ms).exp() * (-error_decay * error_rate).exp();

        // Ensure at least weight 1, and fit in i64
        weight.max(1.0) as i64
    }

    #[instrument(skip(self), name = "select_outbound", level = "debug", ret)]
    fn select(&self, _req: &OutboundRequest) -> &StatsOutbound<O> {
        if self.outbounds.len() == 1 {
            return &self.outbounds[0];
        }

        self.outbounds
            .choose_weighted(&mut rng(), |o| {
                let weight = o
                    .report()
                    .map(|r| {
                        Self::calculate_weight(r.avg_delay.as_millis() as u64, r.error_rate_percent)
                    })
                    .unwrap_or(1);

                tracing::debug!(?weight, outbound=?o, "Calculated outbound");
                weight
            })
            .expect("To be able to choose an outbound")
    }
}

impl<O: Outbound + Debug> Outbound for SelectorOutbound<O> {
    #[instrument(skip(self), name = "send_via_selector", level = "info")]
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin> {
        self.select(&req).send(req).await
    }
}
