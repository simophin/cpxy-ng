use anyhow::{Context, bail, ensure};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout};

/// How long a SOCKS5 server gets to answer each handshake step before we give up.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How many greeted connections to keep ready per SOCKS5 server.
const POOL_SIZE: usize = 4;
/// Pooled connections idle longer than this are discarded rather than used,
/// in case the SOCKS5 server times out connections that haven't sent a request.
const POOL_MAX_IDLE: Duration = Duration::from_secs(30);
/// How often the pool checks for expired connections when nothing is taken.
const POOL_CHECK_INTERVAL: Duration = Duration::from_secs(10);
/// How long to wait before retrying after failing to open pooled connections.
const POOL_RETRY_DELAY: Duration = Duration::from_secs(5);

/// The SOCKS5 server answered the CONNECT with a failure code, e.g. because the
/// target is unreachable. Retrying on another connection won't help.
#[derive(Debug)]
struct Rejected(u8);

impl Display for Rejected {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SOCKS5 CONNECT rejected with code {}", self.0)
    }
}

impl std::error::Error for Rejected {}

/// Connects through one SOCKS5 server (no authentication).
///
/// Once [`warm_up`](Self::warm_up) is called, it keeps a few connections open
/// with the greeting already done, so a request only waits for the CONNECT round
/// trip instead of three (TCP, greeting, CONNECT). The greeting and CONNECT can't
/// simply be pipelined, as some servers drop anything sent before their greeting
/// reply.
#[derive(Debug)]
pub struct Socks5Pool {
    addr: String,
    idle: Mutex<Vec<(TcpStream, Instant)>>,
    /// Wakes the maintainer task when a connection is taken.
    taken: Notify,
}

impl Socks5Pool {
    pub fn new(addr: String) -> Arc<Self> {
        Arc::new(Self {
            addr,
            idle: Mutex::new(Vec::new()),
            taken: Notify::new(),
        })
    }

    /// Starts keeping greeted connections ready. Without this, every request
    /// opens a fresh connection.
    pub fn warm_up(self: &Arc<Self>) {
        tokio::spawn(self.clone().maintain());
    }

    /// Opens a connection to `host:port` through the SOCKS5 server. `host` is
    /// passed through as a domain name so the SOCKS5 server resolves it.
    pub async fn connect(&self, host: &str, port: u16) -> anyhow::Result<TcpStream> {
        ensure!(host.len() <= 255, "Host name too long for SOCKS5");

        if let Some(stream) = self.take() {
            match with_timeout(request(stream, host, port)).await {
                Ok(stream) => return Ok(stream),
                Err(e) if e.downcast_ref::<Rejected>().is_some() => return Err(e),
                // The server may have closed the idle connection: retry on a fresh one.
                Err(e) => tracing::debug!(
                    proxy = self.addr,
                    error = %e,
                    "SOCKS5: pooled connection failed, retrying on a fresh one"
                ),
            }
        }

        let stream = with_timeout(open_greeted(&self.addr)).await?;
        with_timeout(request(stream, host, port)).await
    }

    /// Takes the newest usable idle connection, if any.
    fn take(&self) -> Option<TcpStream> {
        let stream = {
            let mut idle = self.idle.lock().unwrap();
            std::iter::from_fn(|| idle.pop()).find(|(s, at)| is_usable(s, *at))
        };
        self.taken.notify_one();
        stream.map(|(s, _)| s)
    }

    /// Keeps `POOL_SIZE` fresh greeted connections in `idle`.
    async fn maintain(self: Arc<Self>) {
        loop {
            let missing = {
                let mut idle = self.idle.lock().unwrap();
                idle.retain(|(s, at)| is_usable(s, *at));
                POOL_SIZE.saturating_sub(idle.len())
            };

            let mut opening = JoinSet::new();
            for _ in 0..missing {
                let addr = self.addr.clone();
                opening.spawn(async move { with_timeout(open_greeted(&addr)).await });
            }

            let mut failed = false;
            while let Some(result) = opening.join_next().await {
                match result {
                    Ok(Ok(stream)) => self.idle.lock().unwrap().push((stream, Instant::now())),
                    Ok(Err(e)) => {
                        tracing::warn!(proxy = self.addr, error = %e, "SOCKS5: failed to open pooled connection");
                        failed = true;
                    }
                    Err(e) => {
                        tracing::warn!(proxy = self.addr, error = %e, "SOCKS5: pool task failed");
                        failed = true;
                    }
                }
            }

            if failed {
                sleep(POOL_RETRY_DELAY).await;
            } else {
                let _ = timeout(POOL_CHECK_INTERVAL, self.taken.notified()).await;
            }
        }
    }
}

/// Whether an idle connection is young enough and hasn't been closed (or sent
/// unexpected data) by the server.
fn is_usable(stream: &TcpStream, idle_since: Instant) -> bool {
    idle_since.elapsed() < POOL_MAX_IDLE
        && matches!(stream.try_read(&mut [0u8; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

async fn with_timeout<T>(fut: impl Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    timeout(HANDSHAKE_TIMEOUT, fut)
        .await
        .context("SOCKS5 server didn't respond in time")?
}

/// Connects to the SOCKS5 server and completes the no-auth greeting.
async fn open_greeted(addr: &str) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("Error connecting to SOCKS5 server {addr}"))?;
    stream.set_nodelay(true).context("Error setting nodelay")?;

    stream.write_all(&[5, 1, 0]).await?;
    let mut reply = [0u8; 2];
    stream
        .read_exact(&mut reply)
        .await
        .context("Reading method selection")?;
    ensure!(reply[0] == 5, "Unexpected SOCKS version {}", reply[0]);
    ensure!(reply[1] == 0, "SOCKS5 server requires authentication");
    Ok(stream)
}

/// Sends a CONNECT for `host:port` on a greeted connection and reads the reply.
async fn request(mut stream: TcpStream, host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let mut req = Vec::with_capacity(7 + host.len());
    req.extend_from_slice(&[5, 1, 0, 3, host.len() as u8]);
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).await?;

    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .context("Reading CONNECT reply")?;
    ensure!(head[0] == 5, "Unexpected SOCKS version {}", head[0]);
    if head[1] != 0 {
        return Err(Rejected(head[1]).into());
    }

    // Skip the bound address and port.
    let addr_len = match head[3] {
        1 => 4,
        4 => 16,
        3 => stream.read_u8().await? as usize,
        t => bail!("Unknown SOCKS5 address type {t}"),
    };
    let mut skip = vec![0u8; addr_len + 2];
    stream.read_exact(&mut skip).await?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct Stats {
        accepted: AtomicUsize,
        connects: AtomicUsize,
    }

    /// A SOCKS5 server that, like some real ones, ignores anything sent before
    /// its greeting reply. The first `close_idle` connections are closed right
    /// after the greeting (an idle connection dropped by the server); the next
    /// `drop_connect` are closed on receiving the CONNECT (a dead connection that
    /// still looked open). Other CONNECTs get `connect_reply` (0 = success, then
    /// echo).
    async fn spawn_socks5(
        close_idle: usize,
        drop_connect: usize,
        connect_reply: u8,
    ) -> (String, Arc<Stats>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let stats = Arc::new(Stats::default());
        let server_stats = stats.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let n = server_stats.accepted.fetch_add(1, Ordering::SeqCst);
                let stats = server_stats.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    // Read (and drop) whatever arrived with the greeting.
                    s.read(&mut buf).await.unwrap();
                    s.write_all(&[5, 0]).await.unwrap();
                    if n < close_idle {
                        return;
                    }

                    let mut head = [0u8; 5];
                    if s.read_exact(&mut head).await.is_err() {
                        return;
                    }
                    let mut rest = vec![0u8; head[4] as usize + 2];
                    s.read_exact(&mut rest).await.unwrap();
                    if n < close_idle + drop_connect {
                        return;
                    }
                    stats.connects.fetch_add(1, Ordering::SeqCst);

                    s.write_all(&[5, connect_reply, 0, 1, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        (addr, stats)
    }

    /// Puts a greeted connection straight into the pool, bypassing the maintainer.
    async fn push_idle(pool: &Socks5Pool, idle_since: Instant) {
        let s = open_greeted(&pool.addr).await.unwrap();
        pool.idle.lock().unwrap().push((s, idle_since));
    }

    async fn assert_echo(stream: &mut TcpStream) {
        stream.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    async fn wait_for_idle(pool: &Socks5Pool, n: usize) {
        timeout(Duration::from_secs(5), async {
            while pool.idle.lock().unwrap().len() < n {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pool didn't fill");
    }

    #[tokio::test]
    async fn connects_without_warm_up() {
        let (addr, stats) = spawn_socks5(0, 0, 0).await;
        let pool = Socks5Pool::new(addr);

        let mut stream = pool.connect("example.com", 80).await.unwrap();
        assert_echo(&mut stream).await;
        assert_eq!(stats.accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn uses_and_refills_warm_connections() {
        let (addr, stats) = spawn_socks5(0, 0, 0).await;
        let pool = Socks5Pool::new(addr);
        pool.warm_up();
        wait_for_idle(&pool, POOL_SIZE).await;
        assert_eq!(stats.accepted.load(Ordering::SeqCst), POOL_SIZE);

        let mut stream = pool.connect("example.com", 80).await.unwrap();
        assert_echo(&mut stream).await;
        assert_eq!(stats.connects.load(Ordering::SeqCst), 1);
        // The taken connection gets replaced.
        wait_for_idle(&pool, POOL_SIZE).await;
        assert_eq!(stats.accepted.load(Ordering::SeqCst), POOL_SIZE + 1);
    }

    #[tokio::test]
    async fn skips_pooled_connections_the_server_closed() {
        let (addr, stats) = spawn_socks5(POOL_SIZE, 0, 0).await;
        let pool = Socks5Pool::new(addr);
        for _ in 0..POOL_SIZE {
            push_idle(&pool, Instant::now()).await;
        }
        // Let the server's FINs arrive.
        sleep(Duration::from_millis(50)).await;

        let mut stream = pool.connect("example.com", 80).await.unwrap();
        assert_echo(&mut stream).await;
        // Only the fresh connection got as far as a CONNECT.
        assert_eq!(stats.accepted.load(Ordering::SeqCst), POOL_SIZE + 1);
        assert_eq!(stats.connects.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_once_when_a_pooled_connection_fails() {
        let (addr, stats) = spawn_socks5(0, 1, 0).await;
        let pool = Socks5Pool::new(addr);
        push_idle(&pool, Instant::now()).await;

        let mut stream = pool.connect("example.com", 80).await.unwrap();
        assert_echo(&mut stream).await;
        assert_eq!(stats.accepted.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn does_not_retry_rejections() {
        let (addr, stats) = spawn_socks5(0, 0, 4).await;
        let pool = Socks5Pool::new(addr);
        push_idle(&pool, Instant::now()).await;

        let err = pool.connect("example.com", 80).await.unwrap_err();
        assert!(err.downcast_ref::<Rejected>().is_some(), "{err:?}");
        assert_eq!(stats.accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn discards_expired_connections() {
        let (addr, _) = spawn_socks5(0, 0, 0).await;
        let pool = Socks5Pool::new(addr);
        push_idle(&pool, Instant::now() - POOL_MAX_IDLE).await;
        assert!(pool.take().is_none());
    }
}
