//! The TUN file descriptor from the platform, as the device the IP stack reads and writes.
//! Each read and write moves exactly one IP packet.

use crate::filter::{Verdict, classify};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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

/// Applies [`classify`] to every packet before the IP stack sees it, and answers refused
/// packets itself.
pub struct TunDevice {
    fd: AsyncFd<OwnedFd>,
    buf: Vec<u8>,
    traffic: Arc<Traffic>,
}

impl TunDevice {
    pub fn new(fd: OwnedFd, mtu: u16, traffic: Arc<Traffic>) -> io::Result<Self> {
        set_nonblocking(&fd)?;
        Ok(Self {
            fd: AsyncFd::new(fd)?,
            buf: vec![0; usize::from(mtu) + PACKET_INFO_LEN],
            traffic,
        })
    }

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
            let mut guard = ready!(this.fd.poll_read_ready(cx))?;
            let buf = &mut this.buf;
            let read = guard.try_io(|fd| {
                // SAFETY: reading into our own buffer, bounded by its length.
                let n =
                    unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            let n = match read {
                Ok(Ok(0)) => return Poll::Ready(Ok(())),
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                // Spurious wake-up: readiness was cleared, poll again.
                Err(_) => continue,
            };
            if n <= PACKET_INFO_LEN {
                continue;
            }

            let packet = &this.buf[PACKET_INFO_LEN..n];
            this.traffic
                .sent
                .fetch_add(packet.len() as u64, Ordering::Relaxed);
            match classify(packet) {
                Verdict::Pass => {
                    let len = packet.len().min(out.remaining());
                    out.put_slice(&packet[..len]);
                    return Poll::Ready(Ok(()));
                }
                Verdict::Drop => {}
                Verdict::Reply(reply) => {
                    if let Err(e) = this.send_now(&reply) {
                        tracing::debug!("Error writing ICMP reply: {e}");
                    }
                }
            }
        }
    }
}

impl AsyncWrite for TunDevice {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        packet: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.into_ref().get_ref();
        loop {
            let mut guard = ready!(this.fd.poll_write_ready(cx))?;
            match guard.try_io(|_| this.send_now(packet)) {
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
