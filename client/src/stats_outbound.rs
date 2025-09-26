use cpxy_ng::outbound::{Outbound, OutboundRequest};
use std::collections::VecDeque;
use std::fmt::{Debug, Formatter};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};

struct Stat {
    delay: Duration,
    error: bool,
}

pub struct StatsOutbound<O> {
    outbound: O,
    last_stats: RwLock<VecDeque<Stat>>,
}

impl<O: Clone> Clone for StatsOutbound<O> {
    fn clone(&self) -> Self {
        Self {
            outbound: self.outbound.clone(),
            last_stats: Default::default(),
        }
    }
}

impl<O: Debug> Debug for StatsOutbound<O> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatsOutbound")
            .field("inner", &self.outbound)
            .finish()
    }
}

pub struct Report {
    pub avg_delay: Duration,
    pub error_rate_percent: usize,
}

const MAX_STATS: usize = 100;

impl<O> StatsOutbound<O> {
    pub fn new(outbound: O) -> Self {
        Self {
            outbound,
            last_stats: Default::default(),
        }
    }

    pub fn report(&self) -> Option<Report> {
        let stats = self.last_stats.read().unwrap();
        if stats.is_empty() {
            None
        } else {
            Some(Report {
                avg_delay: stats.iter().map(|d| d.delay).sum::<Duration>() / (stats.len() as u32),
                error_rate_percent: stats.iter().filter(|s| s.error).count() * 100 / stats.len(),
            })
        }
    }
}

impl<O> Outbound for StatsOutbound<O>
where
    O: Outbound,
{
    async fn send(
        &self,
        req: OutboundRequest,
    ) -> anyhow::Result<impl AsyncRead + AsyncWrite + Unpin> {
        let start = Instant::now();
        let res = self.outbound.send(req).await;
        let delay = start.elapsed();
        let error = res.is_err();

        {
            let mut stats = self.last_stats.write().unwrap();
            stats.push_back(Stat { delay, error });
            while stats.len() > MAX_STATS {
                stats.pop_front();
            }
        }

        res
    }
}
