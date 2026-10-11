//! Batched reads and writes on a socket descriptor: see the parent module.

use super::{Received, Shared};
use futures::StreamExt;
use futures::channel::mpsc;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, ready};
use tokio::sync::oneshot;

/// The most packets one `recvmmsg` or `sendmmsg` moves.
const BATCH: usize = 64;
/// Packets queued for the send task before writes wait for it.
const QUEUE: usize = 256;

/// Whether `fd` is a socket that keeps message boundaries, where each message is one packet.
pub(super) fn is_message_socket(fd: &OwnedFd) -> bool {
    let mut kind: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: SO_TYPE writes one c_int into `kind`, whose size `len` gives.
    let is_socket = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut kind as *mut _ as *mut _,
            &mut len,
        )
    } == 0;
    is_socket && (kind == libc::SOCK_SEQPACKET || kind == libc::SOCK_DGRAM)
}

pub(super) struct Batch {
    shared: Arc<Shared>,
    /// [`BATCH`] slots of `slot_len` bytes, each receiving one packet.
    slots: Vec<u8>,
    slot_len: usize,
    /// The length of the packet in each slot, and whether it was cut short.
    received: [(usize, bool); BATCH],
    /// The slots filled by the last `recvmmsg`, and the next one to hand out.
    count: usize,
    next: usize,
    queue: mpsc::Sender<Vec<u8>>,
    /// Dropped with the device, which stops the send task.
    _stop: oneshot::Sender<()>,
}

impl Batch {
    /// Starts the send task, so must be called within a Tokio runtime.
    pub(super) fn new(shared: Arc<Shared>, mtu: u16) -> Self {
        let (queue, queued) = mpsc::channel(QUEUE);
        let (stop, stopped) = oneshot::channel();
        tokio::spawn(send_queued(shared.clone(), queued, stopped));
        let slot_len = usize::from(mtu);
        Self {
            shared,
            slots: vec![0; BATCH * slot_len],
            slot_len,
            received: [(0, false); BATCH],
            count: 0,
            next: 0,
            queue,
            _stop: stop,
        }
    }

    /// The next packet received, reading every packet queued in the socket when none is left.
    pub(super) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<Received<'_>>> {
        while self.next == self.count {
            let mut guard = ready!(self.shared.fd.poll_read_ready(cx))?;
            let slots = &mut self.slots;
            let (slot_len, received) = (self.slot_len, &mut self.received);
            match guard.try_io(|fd| recv_batch(fd.as_raw_fd(), slots, slot_len, received)) {
                Ok(Ok(n)) => {
                    self.count = n;
                    self.next = 0;
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                // Spurious wake-up: readiness was cleared, poll again.
                Err(_) => {}
            }
        }
        let i = self.next;
        self.next += 1;
        let (len, truncated) = self.received[i];
        Poll::Ready(Ok(if len == 0 {
            Received::Eof
        } else if truncated {
            // Larger than the MTU, which tsnet never sends.
            Received::Skip
        } else {
            let start = i * self.slot_len;
            Received::Packet(&self.slots[start..start + len])
        }))
    }

    /// Queues one packet for the send task, waiting while the queue is full.
    pub(super) fn poll_queue(
        &mut self,
        cx: &mut Context<'_>,
        packet: &[u8],
    ) -> Poll<io::Result<usize>> {
        let sent = match ready!(self.queue.poll_ready(cx)) {
            Ok(()) => self.queue.start_send(packet.to_vec()),
            Err(e) => Err(e),
        };
        Poll::Ready(match sent {
            Ok(()) => Ok(packet.len()),
            // The send task ended on an error, which it logged.
            Err(_) => Err(io::ErrorKind::BrokenPipe.into()),
        })
    }
}

/// Reads up to [`BATCH`] packets without waiting, one into each slot.
fn recv_batch(
    fd: RawFd,
    slots: &mut [u8],
    slot_len: usize,
    received: &mut [(usize, bool); BATCH],
) -> io::Result<usize> {
    // SAFETY: all-zero is a valid iovec and mmsghdr.
    let mut iovs: [libc::iovec; BATCH] = unsafe { MaybeUninit::zeroed().assume_init() };
    let mut msgs: [libc::mmsghdr; BATCH] = unsafe { MaybeUninit::zeroed().assume_init() };
    for (i, (iov, msg)) in iovs.iter_mut().zip(msgs.iter_mut()).enumerate() {
        iov.iov_base = slots[i * slot_len..].as_mut_ptr() as *mut _;
        iov.iov_len = slot_len;
        msg.msg_hdr.msg_iov = iov;
        msg.msg_hdr.msg_iovlen = 1;
    }
    // SAFETY: each message points at its own slot of `slots`, which outlives the call.
    let n = unsafe {
        libc::recvmmsg(
            fd,
            msgs.as_mut_ptr(),
            BATCH as _,
            libc::MSG_DONTWAIT as _,
            std::ptr::null_mut(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let n = n as usize;
    for (msg, received) in msgs[..n].iter().zip(received.iter_mut()) {
        *received = (
            msg.msg_len as usize,
            msg.msg_hdr.msg_flags & libc::MSG_TRUNC != 0,
        );
    }
    Ok(n)
}

/// Sends the packets queued, as many at a time as are queued, until the device is dropped or
/// sending fails.
async fn send_queued(
    shared: Arc<Shared>,
    mut queued: mpsc::Receiver<Vec<u8>>,
    stopped: oneshot::Receiver<()>,
) {
    let send = async {
        let mut batch = Vec::with_capacity(BATCH);
        while let Some(packet) = queued.next().await {
            batch.push(packet);
            while batch.len() < BATCH {
                match queued.try_recv() {
                    Ok(packet) => batch.push(packet),
                    Err(_) => break,
                }
            }
            if let Err(e) = send_batch(&shared, &batch).await {
                tracing::debug!("Error writing to the device: {e}");
                return;
            }
            batch.clear();
        }
    };
    tokio::select! {
        _ = stopped => {}
        _ = send => {}
    }
}

/// Sends `packets`, waiting while the socket's buffer is full rather than dropping.
async fn send_batch(shared: &Shared, packets: &[Vec<u8>]) -> io::Result<()> {
    let mut sent = 0;
    while sent < packets.len() {
        let mut guard = shared.fd.writable().await?;
        match guard.try_io(|fd| send_mmsg(fd.as_raw_fd(), &packets[sent..])) {
            Ok(Ok(n)) => {
                let bytes: usize = packets[sent..sent + n].iter().map(Vec::len).sum();
                shared
                    .traffic
                    .received
                    .fetch_add(bytes as u64, Ordering::Relaxed);
                sent += n;
            }
            Ok(Err(e)) => return Err(e),
            // The buffer filled: wait for room.
            Err(_) => {}
        }
    }
    Ok(())
}

/// Sends up to [`BATCH`] packets without waiting, returning how many were sent.
fn send_mmsg(fd: RawFd, packets: &[Vec<u8>]) -> io::Result<usize> {
    let count = packets.len().min(BATCH);
    // SAFETY: all-zero is a valid iovec and mmsghdr.
    let mut iovs: [libc::iovec; BATCH] = unsafe { MaybeUninit::zeroed().assume_init() };
    let mut msgs: [libc::mmsghdr; BATCH] = unsafe { MaybeUninit::zeroed().assume_init() };
    for ((packet, iov), msg) in packets.iter().zip(&mut iovs).zip(&mut msgs) {
        iov.iov_base = packet.as_ptr() as *mut _;
        iov.iov_len = packet.len();
        msg.msg_hdr.msg_iov = iov;
        msg.msg_hdr.msg_iovlen = 1;
    }
    // SAFETY: each message points at one packet, which outlives the call; nothing is written.
    let n = unsafe { libc::sendmmsg(fd, msgs.as_mut_ptr(), count as _, libc::MSG_DONTWAIT as _) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}
