//! Pre-connected TCP sockets to the cpxy server.
//!
//! The cpxy protocol has nothing to say before the request: the client's first bytes are the
//! request itself, and the server sends nothing until it has parsed one. So a warm connection is
//! just a connected TCP socket, and taking one saves the TCP handshake round trip.
//!
//! Warming is driven by use. The first request starts it, and while requests keep coming the pool
//! holds [`TARGET_IDLE`] sockets. Once nothing has been taken for [`ACTIVE_WINDOW`], the pool closes
//! its sockets and stops, so an idle client doesn't hold connections open (which would also keep
//! an auto-stopping server awake).

use anyhow::Context;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{Instant, sleep, timeout};

/// How many connected sockets to keep ready while the pool is active.
const TARGET_IDLE: usize = 2;
/// Sockets idle longer than this are closed rather than used, staying under the idle timeouts of
/// NATs and proxies in front of the server. The cpxy server itself never times out a connection
/// waiting for its request.
const MAX_IDLE: Duration = Duration::from_secs(20);
/// Keep warming while a socket was taken within this long.
const ACTIVE_WINDOW: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait before trying again after failing to connect.
const RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct WarmPool {
    host: String,
    port: u16,
    state: Mutex<State>,
    /// Wakes the maintainer when a socket is taken.
    taken: Notify,
}

#[derive(Debug, Default)]
struct State {
    idle: Vec<(TcpStream, Instant)>,
    last_taken: Option<Instant>,
    maintaining: bool,
}

impl WarmPool {
    pub fn new(host: String, port: u16) -> Arc<Self> {
        Arc::new(Self {
            host,
            port,
            state: Mutex::default(),
            taken: Notify::new(),
        })
    }

    /// Takes a warm socket if one is ready, and marks the pool as in use so it keeps (or starts)
    /// warming. Must be called on a Tokio runtime.
    pub fn take(self: &Arc<Self>) -> Option<TcpStream> {
        let socket = {
            let mut state = self.state.lock().unwrap();
            state.last_taken = Some(Instant::now());
            if !state.maintaining {
                state.maintaining = true;
                tokio::spawn(maintain(Arc::downgrade(self)));
            }
            std::iter::from_fn(|| state.idle.pop()).find(|(s, at)| is_usable(s, *at))
        };
        self.taken.notify_one();
        socket.map(|(s, _)| s)
    }

    /// Opens a new connection to the server.
    pub async fn connect(&self) -> anyhow::Result<TcpStream> {
        let socket = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .with_context(|| {
                format!(
                    "Error connecting to upstream server: {}:{}",
                    self.host, self.port
                )
            })?;
        socket
            .set_nodelay(true)
            .context("Error setting nodelay on TCP stream")?;
        Ok(socket)
    }

    /// Drops expired sockets and returns how many to open, or `None` once the pool has gone
    /// unused for [`ACTIVE_WINDOW`], after closing its sockets and marking it stopped.
    fn prune(&self) -> Option<usize> {
        let mut state = self.state.lock().unwrap();
        if state
            .last_taken
            .is_none_or(|at| at.elapsed() >= ACTIVE_WINDOW)
        {
            state.idle.clear();
            state.maintaining = false;
            return None;
        }
        state.idle.retain(|(s, at)| is_usable(s, *at));
        Some(TARGET_IDLE.saturating_sub(state.idle.len()))
    }

    /// When the oldest idle socket expires, if there is one.
    fn next_expiry(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap();
        state.idle.iter().map(|(_, at)| *at + MAX_IDLE).min()
    }
}

/// Keeps the pool topped up while it's in use. Holds the pool only weakly between rounds, so
/// dropping the outbound stops it.
async fn maintain(pool: Weak<WarmPool>) {
    loop {
        let Some(this) = pool.upgrade() else { return };
        let Some(missing) = this.prune() else {
            tracing::debug!(host = this.host, "Warm pool: unused, closing idle connections");
            return;
        };

        let opened = futures::future::join_all(
            (0..missing).map(|_| timeout(CONNECT_TIMEOUT, this.connect())),
        )
        .await;

        let mut failed = false;
        for result in opened {
            match result.map_err(anyhow::Error::from).and_then(|r| r) {
                Ok(socket) => this
                    .state
                    .lock()
                    .unwrap()
                    .idle
                    .push((socket, Instant::now())),
                Err(e) => {
                    tracing::debug!(host = this.host, error = %e, "Warm pool: failed to pre-connect");
                    failed = true;
                }
            }
        }

        if failed {
            // Not woken by takes, so a busy client doesn't hammer a server that's down.
            sleep(RETRY_DELAY).await;
            continue;
        }

        // Wake for the next take, the next expiry, or the end of the active window.
        let window_end = this.state.lock().unwrap().last_taken.map(|at| at + ACTIVE_WINDOW);
        let wake = [this.next_expiry(), window_end]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or_else(Instant::now);
        let _ = timeout(
            wake.saturating_duration_since(Instant::now()),
            this.taken.notified(),
        )
        .await;
    }
}

/// Whether an idle socket is young enough and still open. The server never sends anything
/// before the request, so readable data or EOF means the connection is dead.
fn is_usable(socket: &TcpStream, idle_since: Instant) -> bool {
    idle_since.elapsed() < MAX_IDLE
        && matches!(socket.try_read(&mut [0u8; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn stays_cold_until_first_take() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pool = WarmPool::new("127.0.0.1".into(), listener.local_addr().unwrap().port());
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "connected before anything was taken"
        );

        assert!(pool.take().is_none());
        let _accepted = timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("didn't warm after a take")
            .unwrap();
    }

    #[tokio::test]
    async fn closed_sockets_are_not_handed_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pool = WarmPool::new("127.0.0.1".into(), listener.local_addr().unwrap().port());
        assert!(pool.take().is_none());
        for _ in 0..TARGET_IDLE {
            // The server end closes each warm connection straight away.
            drop(listener.accept().await.unwrap());
        }
        timeout(Duration::from_secs(5), async {
            while pool.state.lock().unwrap().idle.len() < TARGET_IDLE {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert!(pool.take().is_none());
    }

    #[tokio::test]
    async fn closes_sockets_once_unused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pool = WarmPool::new("127.0.0.1".into(), listener.local_addr().unwrap().port());
        pool.take();
        let mut accepted = Vec::new();
        for _ in 0..TARGET_IDLE {
            accepted.push(listener.accept().await.unwrap().0);
        }

        pool.state.lock().unwrap().last_taken = Some(Instant::now() - ACTIVE_WINDOW);
        assert_eq!(pool.prune(), None);
        let state = pool.state.lock().unwrap();
        assert!(state.idle.is_empty() && !state.maintaining);
    }
}
