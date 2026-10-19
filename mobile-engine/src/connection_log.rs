//! The shared engine connection records and bounded, paged history.

use crate::OutboundEvent;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A TCP flow connected through an outbound, or failing to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, uniffi::Record)]
pub struct ConnectionEvent {
    /// The server name the app asked for, or the address when it named none.
    pub host: String,
    /// The address connected to, when known.
    pub ip: Option<String>,
    pub port: u16,
    /// `direct` or `proxy`.
    pub outbound: String,
    /// How long connecting took.
    pub delay_millis: u64,
    /// When the flow was opened, in milliseconds since the Unix epoch.
    pub time_millis: u64,
    /// Set when connecting failed.
    pub error: Option<String>,
}

impl From<OutboundEvent> for ConnectionEvent {
    fn from(event: OutboundEvent) -> Self {
        match event {
            OutboundEvent::Connected {
                host,
                ip,
                port,
                outbound,
                delay_mills,
                request_time_mills,
            } => Self {
                ip: ip.map(|ip| ip.to_string()),
                host,
                port,
                outbound: outbound.into_owned(),
                delay_millis: delay_mills as u64,
                time_millis: request_time_mills,
                error: None,
            },
            OutboundEvent::Error {
                host,
                ip,
                port,
                outbound,
                delay_mills,
                request_time_mills,
                error,
            } => Self {
                ip: ip.map(|ip| ip.to_string()),
                host,
                port,
                outbound: outbound.into_owned(),
                delay_millis: delay_mills as u64,
                time_millis: request_time_mills,
                error: Some(error),
            },
        }
    }
}

/// How many connections are kept; the oldest are dropped beyond it.
pub const CAPACITY: usize = 10_000;

/// Shared by every log in the process, so a log started after another one never reuses a
/// sequence number the app has seen.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, uniffi::Record)]
pub struct ConnectionRecord {
    /// Increases with every connection; unique within the process.
    pub seq: u64,
    pub event: ConnectionEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, uniffi::Record)]
pub struct ConnectionPage {
    /// Oldest first.
    pub records: Vec<ConnectionRecord>,
    /// The `seq` of the oldest record still kept, or of the next one when none are. Records
    /// before it are gone.
    pub oldest_seq: u64,
}

pub struct ConnectionLog {
    records: Mutex<VecDeque<ConnectionRecord>>,
    capacity: usize,
}

impl ConnectionLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            records: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    pub fn push(&self, event: ConnectionEvent) {
        let mut records = self.records.lock().unwrap();
        if records.len() == self.capacity {
            records.pop_front();
        }
        // Taken under the lock, so the records stay in order.
        let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
        records.push_back(ConnectionRecord { seq, event });
    }

    /// The oldest `limit` records from `since` on.
    pub fn since(&self, since: u64, limit: usize) -> ConnectionPage {
        let records = self.records.lock().unwrap();
        let start = records.partition_point(|r| r.seq < since);
        page(&records, start, (start + limit).min(records.len()))
    }

    /// The newest `limit` records before `before`, or the newest of all when it is `None`.
    pub fn before(&self, before: Option<u64>, limit: usize) -> ConnectionPage {
        let records = self.records.lock().unwrap();
        let end = before.map_or(records.len(), |before| {
            records.partition_point(|r| r.seq < before)
        });
        page(&records, end.saturating_sub(limit), end)
    }
}

fn page(records: &VecDeque<ConnectionRecord>, start: usize, end: usize) -> ConnectionPage {
    ConnectionPage {
        records: records.range(start..end).cloned().collect(),
        oldest_seq: records
            .front()
            .map_or_else(|| NEXT_SEQ.load(Ordering::Relaxed), |r| r.seq),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::net::Ipv4Addr;

    #[test]
    fn converts_outbound_events() {
        let event: ConnectionEvent = OutboundEvent::Error {
            host: "one.one.one.one".into(),
            ip: Some(Ipv4Addr::new(1, 0, 0, 1).into()),
            port: 443,
            outbound: Cow::Borrowed("proxy"),
            delay_mills: 12,
            request_time_mills: 34,
            error: "refused".into(),
        }
        .into();
        assert_eq!(
            event,
            ConnectionEvent {
                host: "one.one.one.one".into(),
                ip: Some("1.0.0.1".into()),
                port: 443,
                outbound: "proxy".into(),
                delay_millis: 12,
                time_millis: 34,
                error: Some("refused".into()),
            }
        );
    }

    fn event(port: u16) -> ConnectionEvent {
        ConnectionEvent {
            host: "1.2.3.4".into(),
            ip: Some("1.2.3.4".into()),
            port,
            outbound: "proxy".into(),
            delay_millis: 1,
            time_millis: 2,
            error: None,
        }
    }

    fn ports(page: &ConnectionPage) -> Vec<u16> {
        page.records.iter().map(|r| r.event.port).collect()
    }

    /// The sequence is shared with the tests running alongside, so the seqs are not contiguous;
    /// returns them with the log.
    fn filled(capacity: usize, ports: impl IntoIterator<Item = u16>) -> (ConnectionLog, Vec<u64>) {
        let log = ConnectionLog::new(capacity);
        for port in ports {
            log.push(event(port));
        }
        let seqs = log
            .before(None, capacity)
            .records
            .iter()
            .map(|r| r.seq)
            .collect();
        (log, seqs)
    }

    #[test]
    fn pages_forward() {
        let (log, seqs) = filled(10, 1..=5);
        let page = log.since(seqs[0], 2);
        assert_eq!(ports(&page), [1, 2]);
        assert_eq!(page.oldest_seq, seqs[0]);
        assert_eq!(ports(&log.since(seqs[2], 10)), [3, 4, 5]);
        assert_eq!(ports(&log.since(seqs[4] + 1, 10)), [] as [u16; 0]);
    }

    #[test]
    fn pages_backward() {
        let (log, seqs) = filled(10, 1..=5);
        assert_eq!(ports(&log.before(None, 2)), [4, 5]);
        assert_eq!(ports(&log.before(Some(seqs[3]), 2)), [2, 3]);
        assert_eq!(ports(&log.before(Some(seqs[1]), 10)), [1]);
        assert_eq!(ports(&log.before(Some(seqs[0]), 10)), [] as [u16; 0]);
    }

    #[test]
    fn drops_the_oldest_beyond_the_capacity() {
        let (log, seqs) = filled(3, 1..=3);
        log.push(event(4));
        let page = log.since(seqs[0], 10);
        // Asking from before the oldest kept starts at it, and `oldest_seq` shows the gap.
        assert_eq!(ports(&page), [2, 3, 4]);
        assert_eq!(page.oldest_seq, seqs[1]);
    }

    #[test]
    fn a_new_log_continues_the_sequence() {
        let (_, first) = filled(10, [1]);
        let (_, next) = filled(10, [2]);
        assert!(next[0] > first[0]);
    }
}
