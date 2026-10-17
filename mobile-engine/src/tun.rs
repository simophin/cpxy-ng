//! The TUN file descriptor from the platform, as the device the IP stack reads and writes.
//! Each read and write moves exactly one IP packet.
//!
//! On Linux the descriptor may instead be a datagram or seqpacket socket (cpxy-tsgw's socketpair
//! to tsnet). There packets move in batches, as tsnet does on its side: a read takes every queued
//! packet with one `recvmmsg`, and writes are queued for a task that sends them with `sendmmsg`.
//! A burst then costs a few system calls and wake-ups on each side instead of several per packet.

use crate::filter::{Filter, Verdict};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Notify, oneshot};

#[cfg(any(target_os = "linux", target_os = "android"))]
mod batch;

/// iOS and macOS utun devices prefix every packet with the address family.
const PACKET_INFO: bool = cfg!(any(target_os = "ios", target_os = "macos"));
const PACKET_INFO_LEN: usize = if PACKET_INFO { 4 } else { 0 };

/// Bytes in and out of the tunnel, counted at the IP layer.
#[derive(Default)]
pub struct Traffic {
    /// From the device's apps into the tunnel.
    pub sent: AtomicU64,
    /// From the tunnel back to the apps.
    pub received: AtomicU64,
}

/// Applies a [`Filter`] to every packet before the IP stack sees it, and answers refused packets
/// itself.
pub struct TunDevice {
    shared: Arc<Shared>,
    filter: Filter,
    buf: Vec<u8>,
    /// Notified when the other end of a socket closes; a TUN never does.
    eof: Arc<Notify>,
    /// Set when the descriptor is a socket that moves packets in batches.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    batch: Option<batch::Batch>,
}

/// What the device shares with its send task.
struct Shared {
    fd: AsyncFd<OwnedFd>,
    traffic: Arc<Traffic>,
    /// Dropped after `fd`, so its receiver learns that the descriptor is closed.
    _closed: oneshot::Sender<()>,
}

impl TunDevice {
    /// Must be called within a Tokio runtime, which runs the send task of a socket.
    pub fn new(
        fd: OwnedFd,
        mtu: u16,
        filter: Filter,
        traffic: Arc<Traffic>,
        eof: Arc<Notify>,
        closed: oneshot::Sender<()>,
    ) -> io::Result<Self> {
        set_nonblocking(&fd)?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let is_socket = batch::is_message_socket(&fd);
        let shared = Arc::new(Shared {
            fd: AsyncFd::new(fd)?,
            traffic,
            _closed: closed,
        });
        Ok(Self {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            batch: is_socket.then(|| batch::Batch::new(shared.clone(), mtu)),
            shared,
            filter,
            buf: vec![0; usize::from(mtu) + PACKET_INFO_LEN],
            eof,
        })
    }
}

impl Shared {
    /// Writes one packet without waiting. The kernel queues TUN writes, so this only fails when
    /// the queue is full, and the packet is then lost like on any congested link.
    fn send_now(&self, packet: &[u8]) -> io::Result<usize> {
        let header = packet_info(packet);
        let iov = [
            libc::iovec {
                iov_base: header.as_ptr() as *mut _,
                iov_len: PACKET_INFO_LEN,
            },
            libc::iovec {
                iov_base: packet.as_ptr() as *mut _,
                iov_len: packet.len(),
            },
        ];
        // SAFETY: both buffers outlive the call and the lengths match them.
        let n = unsafe { libc::writev(self.fd.as_raw_fd(), iov.as_ptr(), iov.len() as _) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        self.traffic
            .received
            .fetch_add(packet.len() as u64, Ordering::Relaxed);
        Ok(packet.len())
    }
}

fn packet_info(packet: &[u8]) -> [u8; 4] {
    let family = match packet.first().map(|b| b >> 4) {
        Some(6) => libc::AF_INET6,
        _ => libc::AF_INET,
    };
    (family as u32).to_be_bytes()
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: plain fcntl calls on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

impl AsyncRead for TunDevice {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let received = match this.batch.as_mut() {
                Some(batch) => ready!(batch.poll_next(cx))?,
                None => ready!(poll_read_one(&this.shared, &mut this.buf, cx))?,
            };
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let received = ready!(poll_read_one(&this.shared, &mut this.buf, cx))?;
            let packet = match received {
                Received::Packet(packet) => packet,
                Received::Skip => continue,
                Received::Eof => {
                    // The other end of a socket closed. The IP stack would take every empty read
                    // as a packet and never stop, so it gets nothing more; the engine stops.
                    this.eof.notify_one();
                    return Poll::Pending;
                }
            };

            this.shared
                .traffic
                .sent
                .fetch_add(packet.len() as u64, Ordering::Relaxed);
            match this.filter.classify(packet) {
                Verdict::Pass => {
                    let len = packet.len().min(out.remaining());
                    out.put_slice(&packet[..len]);
                    return Poll::Ready(Ok(()));
                }
                Verdict::Drop => {}
                Verdict::Reply(reply) => {
                    if let Err(e) = this.shared.send_now(&reply) {
                        tracing::debug!("Error writing ICMP reply: {e}");
                    }
                }
            }
        }
    }
}

/// One read from the device.
enum Received<'a> {
    Packet(&'a [u8]),
    /// Nothing usable: read again.
    Skip,
    /// The other end of a socket closed.
    Eof,
}

/// Reads one packet into `buf`.
fn poll_read_one<'a>(
    shared: &Shared,
    buf: &'a mut [u8],
    cx: &mut Context<'_>,
) -> Poll<io::Result<Received<'a>>> {
    loop {
        let mut guard = ready!(shared.fd.poll_read_ready(cx))?;
        let read = guard.try_io(|fd| {
            // SAFETY: reading into our own buffer, bounded by its length.
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        });
        return Poll::Ready(match read {
            Ok(Ok(0)) => Ok(Received::Eof),
            Ok(Ok(n)) if n <= PACKET_INFO_LEN => Ok(Received::Skip),
            Ok(Ok(n)) => Ok(Received::Packet(&buf[PACKET_INFO_LEN..n])),
            Ok(Err(e)) => Err(e),
            // Spurious wake-up: readiness was cleared, poll again.
            Err(_) => continue,
        });
    }
}

impl AsyncWrite for TunDevice {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        packet: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(batch) = this.batch.as_mut() {
            return batch.poll_queue(cx, packet);
        }
        let shared = &this.shared;
        loop {
            let mut guard = ready!(shared.fd.poll_write_ready(cx))?;
            match guard.try_io(|_| shared.send_now(packet)) {
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
