//! Bidirectional relay that cannot get stuck on a half-closed connection.
//!
//! `tokio::io::copy_bidirectional` returns only after both directions reach EOF. Over a
//! WebSocket tunnel that never happens reliably: shutting down one direction sends a
//! WebSocket Close, which ends the tunnel for both directions, while the other end of the
//! relay may keep its half open indefinitely. Every such connection then pins its sockets
//! and buffers forever.
//!
//! [`relay`] forwards a half-close like `copy_bidirectional` does, but once one direction
//! has finished, the other is only kept alive while it still moves data: after `linger`
//! without any traffic, the whole relay is dropped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{sleep, timeout};

/// How long a half-closed relay may stay idle before it is torn down.
pub const DEFAULT_LINGER: Duration = Duration::from_secs(30);

const BUF_SIZE: usize = 8 * 1024;

/// Relays data between `a` and `b` until both directions finish, either fails, or one
/// direction has finished and the relay then sees no traffic for `linger`.
pub async fn relay<A, B>(a: A, b: B, linger: Duration) -> std::io::Result<()>
where
    A: AsyncRead + AsyncWrite,
    B: AsyncRead + AsyncWrite,
{
    let (mut a_read, mut a_write) = tokio::io::split(a);
    let (mut b_read, mut b_write) = tokio::io::split(b);
    let activity = AtomicU64::new(0);

    let a_to_b = pipe(&mut a_read, &mut b_write, &activity, linger);
    let b_to_a = pipe(&mut b_read, &mut a_write, &activity, linger);
    tokio::pin!(a_to_b, b_to_a);

    let (mut a_done, mut b_done) = (false, false);
    let mut seen = 0;
    while !(a_done && b_done) {
        tokio::select! {
            r = &mut a_to_b, if !a_done => {
                r?;
                a_done = true;
                seen = activity.load(Ordering::Relaxed);
            }
            r = &mut b_to_a, if !b_done => {
                r?;
                b_done = true;
                seen = activity.load(Ordering::Relaxed);
            }
            _ = sleep(linger), if a_done || b_done => {
                let now = activity.load(Ordering::Relaxed);
                if now == seen {
                    return Ok(());
                }
                seen = now;
            }
        }
    }
    Ok(())
}

async fn pipe<R, W>(
    r: &mut R,
    w: &mut W,
    activity: &AtomicU64,
    linger: Duration,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = [0u8; BUF_SIZE];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        activity.fetch_add(1, Ordering::Relaxed);
        w.write_all(&buf[..n]).await?;
        w.flush().await?;
    }
    // Forward the half-close. A peer that never completes it must not hold the relay.
    let _ = timeout(linger, w.shutdown()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn relays_both_directions_and_ends_on_full_close() {
        let (mut client, a) = duplex(64);
        let (b, mut server) = duplex(64);
        let task = tokio::spawn(relay(a, b, Duration::from_secs(60)));

        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        server.write_all(b"pong").await.unwrap();
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");

        client.shutdown().await.unwrap();
        server.shutdown().await.unwrap();
        timeout(Duration::from_secs(5), task)
            .await
            .expect("relay should end once both sides close")
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn half_close_keeps_flowing_while_active() {
        let (mut client, a) = duplex(64);
        let (b, mut server) = duplex(64);
        let linger = Duration::from_secs(10);
        let task = tokio::spawn(relay(a, b, linger));

        // Client finishes its request; the server keeps answering slower than once per
        // linger in total, but never idles longer than linger.
        client.shutdown().await.unwrap();
        let mut eof = [0u8; 1];
        assert_eq!(server.read(&mut eof).await.unwrap(), 0);

        for _ in 0..5 {
            sleep(linger / 2).await;
            server.write_all(b"x").await.unwrap();
            let mut buf = [0u8; 1];
            client.read_exact(&mut buf).await.unwrap();
        }
        assert!(!task.is_finished());

        server.shutdown().await.unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_half_open_relay_is_torn_down() {
        let (_client, a) = duplex(64);
        let (b, mut server) = duplex(64);
        let task = tokio::spawn(relay(a, b, Duration::from_secs(10)));

        // The server side is done, but the client never closes its half.
        server.shutdown().await.unwrap();
        timeout(Duration::from_secs(30), task)
            .await
            .expect("idle half-open relay should be dropped after linger")
            .unwrap()
            .unwrap();
    }
}
