//! The TUN file descriptor from the platform. Each read and write moves exactly one IP packet.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

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

/// The device, shared by everything that writes to it. The descriptor closes with the last clone.
#[derive(Clone)]
pub struct Tun(Arc<Shared>);

struct Shared {
    fd: AsyncFd<OwnedFd>,
    /// Dropped after `fd`, so its receiver learns that the descriptor is closed.
    _closed: oneshot::Sender<()>,
}

impl Tun {
    /// Must be called within a Tokio runtime.
    pub fn new(fd: OwnedFd, closed: oneshot::Sender<()>) -> io::Result<Self> {
        set_nonblocking(&fd)?;
        // SAFETY: the OwnedFd is open and owned by the AsyncFd, which closes it when dropped.
        let fd = unsafe { AsyncFd::register(fd) }.map_err(|e| e.into_parts().1)?;
        Ok(Self(Arc::new(Shared {
            fd,
            _closed: closed,
        })))
    }

    /// Reads one packet into `buf`: its length, or None once the other end of a socket closes (a
    /// TUN never does).
    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let mut info = [0u8; 4];
        loop {
            let mut guard = self.0.fd.readable().await?;
            let read = guard.try_io(|fd| {
                let iov = [
                    libc::iovec {
                        iov_base: info.as_mut_ptr() as *mut _,
                        iov_len: PACKET_INFO_LEN,
                    },
                    libc::iovec {
                        iov_base: buf.as_mut_ptr() as *mut _,
                        iov_len: buf.len(),
                    },
                ];
                // SAFETY: both buffers outlive the call and the lengths match them.
                let n = unsafe { libc::readv(fd.as_raw_fd(), iov.as_ptr(), iov.len() as _) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match read {
                Ok(Ok(0)) => return Ok(None),
                Ok(Ok(n)) => match n.saturating_sub(PACKET_INFO_LEN) {
                    // Nothing but the packet information: read again.
                    0 => continue,
                    len => return Ok(Some(len)),
                },
                Ok(Err(e)) => return Err(e),
                // Spurious wake-up: readiness was cleared, wait again.
                Err(_) => continue,
            }
        }
    }

    /// Writes one packet. The kernel queues TUN writes, so this only waits while the queue is
    /// full.
    pub async fn send(&self, packet: &[u8]) -> io::Result<()> {
        loop {
            let mut guard = self.0.fd.writable().await?;
            match guard.try_io(|fd| write(fd.as_raw_fd(), packet)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    }
}

fn write(fd: i32, packet: &[u8]) -> io::Result<()> {
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
    let n = unsafe { libc::writev(fd, iov.as_ptr(), iov.len() as _) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
