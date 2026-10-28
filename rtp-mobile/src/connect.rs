//! Stagger resolver-ordered candidates, with at most two live allocations (the
//! server's per-UUID quota). A candidate succeeds only after UDP path validation.
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

// Start with the resolver's first family and retain its ordering within each
// family, interleaving so multiple AAAA/A records cannot starve the alternative.
fn candidates(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let Some(first) = addresses.first() else {
        return addresses;
    };
    let first_is_v6 = first.is_ipv6();
    let (a, b): (Vec<_>, Vec<_>) = addresses
        .into_iter()
        .partition(|a| a.is_ipv6() == first_is_v6);
    let (mut a, mut b) = (a.into_iter(), b.into_iter());
    let mut result = Vec::new();
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (a, b) => {
                result.extend(a);
                result.extend(b);
            }
        }
    }
    result
}

pub(crate) fn race<T, F>(
    addresses: Vec<SocketAddr>,
    cancelled: Arc<AtomicBool>,
    attempt: F,
) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: Fn(SocketAddr, Arc<AtomicBool>) -> anyhow::Result<T> + Send + Sync + 'static,
{
    race_with_timing(
        candidates(addresses),
        attempt,
        cancelled,
        Duration::from_millis(250),
        Duration::from_secs(30),
    )
}

fn race_with_timing<T, F>(
    addresses: Vec<SocketAddr>,
    attempt: F,
    caller_cancelled: Arc<AtomicBool>,
    stagger: Duration,
    timeout: Duration,
) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: Fn(SocketAddr, Arc<AtomicBool>) -> anyhow::Result<T> + Send + Sync + 'static,
{
    anyhow::ensure!(!addresses.is_empty(), "No endpoint addresses");
    let cancelled = Cancel(Arc::new(AtomicBool::new(false)));
    let attempt = Arc::new(attempt);
    let (tx, rx) = mpsc::channel();
    let deadline = Instant::now() + timeout;
    let mut next_launch = Instant::now();
    let mut pending = addresses.into_iter().peekable();
    let mut active = 0;
    let mut errors = Vec::new();
    loop {
        anyhow::ensure!(
            !caller_cancelled.load(Ordering::Acquire),
            "Preparation cancelled"
        );
        let now = Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "Endpoint connection deadline exceeded: {}",
                errors
                    .iter()
                    .map(|e: &anyhow::Error| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        if active < 2 && pending.peek().is_some() && (active == 0 || now >= next_launch) {
            let peer = pending.next().unwrap();
            let attempt = attempt.clone();
            let tx = tx.clone();
            let cancel = cancelled.0.clone();
            std::thread::Builder::new()
                .name("rtp-connect".into())
                .spawn(move || {
                    // If the receiver has selected a winner or timed out, dropping
                    // this result releases any successful but unselected allocation.
                    let result = attempt(peer, cancel);
                    let _ = tx.send((peer, result));
                })?;
            active += 1;
            next_launch = now + stagger;
        }
        let until = if active < 2 && pending.peek().is_some() {
            next_launch.min(deadline)
        } else {
            deadline
        };
        match rx.recv_timeout(
            until
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20)),
        ) {
            Ok((_, Ok(winner))) => {
                anyhow::ensure!(
                    !caller_cancelled.load(Ordering::Acquire),
                    "Preparation cancelled"
                );
                return Ok(winner);
            }
            Ok((_peer, Err(error))) => {
                active -= 1;
                errors.push(error);
                if active == 0 && pending.peek().is_none() {
                    return Err(errors.pop().unwrap().context("No usable endpoint"));
                }
                // A known failure need not consume the remainder of the stagger.
                next_launch = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Endpoint attempts disconnected")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    fn v4() -> SocketAddr {
        "127.0.0.1:4000".parse().unwrap()
    }
    fn v6() -> SocketAddr {
        "[::1]:4000".parse().unwrap()
    }

    #[test]
    fn interleaves_without_reordering_within_families() {
        let a: SocketAddr = "[::2]:4000".parse().unwrap();
        let b: SocketAddr = "127.0.0.2:4000".parse().unwrap();
        assert_eq!(candidates(vec![v6(), a, v4(), b]), vec![v6(), v4(), a, b]);
        assert_eq!(candidates(vec![v4(), b, v6(), a]), vec![v4(), v6(), b, a]);
        assert!(candidates(vec![]).is_empty());
    }

    #[test]
    fn preserves_order_for_either_family() {
        for peers in [vec![v6(), v4()], vec![v4(), v6()]] {
            let first = peers[0];
            assert_eq!(
                race(peers, Arc::new(AtomicBool::new(false)), |peer, _| Ok(peer)).unwrap(),
                first
            );
        }
    }

    #[test]
    fn stalled_first_path_does_not_block_second_and_loser_is_cancelled() {
        let (tx, rx) = mpsc::channel();
        let result = race_with_timing(
            vec![v6(), v4()],
            move |peer, cancel| {
                if peer == v6() {
                    while !cancel.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    tx.send(()).unwrap();
                    anyhow::bail!("cancelled UDP path");
                }
                Ok(peer)
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(10),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(result, v4());
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn tries_remaining_addresses_after_failure_with_at_most_two_attempts() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let third: SocketAddr = "127.0.0.2:4000".parse().unwrap();
        let result = race(
            vec![v4(), v6(), third],
            Arc::new(AtomicBool::new(false)),
            move |peer, _| {
                record.lock().unwrap().push(peer);
                if peer != third {
                    anyhow::bail!("unreachable");
                }
                Ok(peer)
            },
        )
        .unwrap();
        assert_eq!(result, third);
        assert_eq!(*seen.lock().unwrap(), vec![v4(), v6(), third]);
    }

    #[test]
    fn late_success_is_dropped_after_winner_returns() {
        struct Lease(mpsc::Sender<()>);
        impl Drop for Lease {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let (tx, rx) = mpsc::channel();
        let winner = race_with_timing(
            vec![v6(), v4()],
            move |peer, _| {
                if peer == v6() {
                    std::thread::sleep(Duration::from_millis(40));
                }
                Ok(Lease(tx.clone()))
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(5),
            Duration::from_secs(1),
        )
        .unwrap();
        // The second candidate wins while the first is still in HTTP setup.
        // Once that setup completes, its lease must not escape through the channel.
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(winner);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn caller_can_cancel_during_a_blocked_attempt() {
        let cancel = Arc::new(AtomicBool::new(false));
        let caller = cancel.clone();
        let (started, rx) = mpsc::channel();
        let task = std::thread::spawn(move || {
            race(vec![v4()], caller, move |_, flag| -> anyhow::Result<()> {
                started.send(()).unwrap();
                while !flag.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                anyhow::bail!("cancelled")
            })
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.store(true, Ordering::Release);
        assert!(task.join().unwrap().is_err());
    }

    #[test]
    fn deadline_cancels_pending_work() {
        let (tx, rx) = mpsc::channel();
        let result = race_with_timing(
            vec![v4()],
            move |_, cancel| -> anyhow::Result<()> {
                while !cancel.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                tx.send(()).unwrap();
                anyhow::bail!("cancelled")
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(1),
            Duration::from_millis(20),
        );
        assert!(result.is_err());
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
