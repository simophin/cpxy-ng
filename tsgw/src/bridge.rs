//! Carries the exit node's forwarded packets between Tailscale and the packet engine.
//!
//! The engine reads and writes IP packets on one end of a SOCK_SEQPACKET socketpair, as it would
//! a TUN, and moves them in batches (recvmmsg and sendmmsg) on its side. This end moves the
//! packets peers send to the internet into the socket, and the engine's replies back to
//! Tailscale, which encrypts each for the peer that owns its destination. This end moves them in
//! batches too, so a burst costs a few system calls each way rather than one per packet.

use anyhow::Context;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use tailscale::{ForwardedPacketReceiver, ForwardedPacketSender, ForwardedPackets, PacketMut};

use tokio::io::unix::AsyncFd;

/// The most packets one recvmmsg or sendmmsg moves, as wireguard-go's batch on Linux.
const BATCH: usize = 128;
/// What each end asks for as its send and receive buffer: room for a few hundred packets in
/// flight each way. The kernel caps it at net.core.wmem_max and rmem_max.
const SOCKET_BUFFER: libc::c_int = 1 << 20;

/// Creates the socketpair: this end, and the engine's, which the engine takes over.
pub fn socketpair() -> anyhow::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: socketpair writes two descriptors into the array.
    let r = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error()).context("socketpair");
    }
    // SAFETY: both descriptors are new and owned by nothing else.
    let (ours, engine) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&ours, &engine] {
        // Best effort: smaller buffers only mean smaller bursts.
        for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            // SAFETY: setting an int option on a socket we own.
            unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    opt,
                    &SOCKET_BUFFER as *const _ as *const _,
                    size_of::<libc::c_int>() as _,
                );
            }
        }
    }
    // SAFETY: plain fcntl calls on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(ours.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(ours.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error()).context("set non-blocking");
        }
    }
    Ok((ours, engine))
}

/// Moves packets both ways until either side closes. `mtu` bounds what the engine sends.
pub async fn run(fd: OwnedFd, packets: ForwardedPackets, mtu: u16) -> anyhow::Result<()> {
    // SAFETY: the AsyncFd owns the descriptor, which stays open until it is dropped.
    let fd = unsafe { AsyncFd::register(fd) }.map_err(io::Error::from)?;
    let (to_peers, from_peers) = packets.split();
    tokio::select! {
        r = to_tailscale(&fd, to_peers, mtu) => r.context("engine to Tailscale"),
        r = to_engine(&fd, from_peers) => r.context("Tailscale to engine"),
    }
}

async fn to_tailscale(
    fd: &AsyncFd<OwnedFd>,
    peers: ForwardedPacketSender,
    mtu: u16,
) -> anyhow::Result<()> {
    let mtu = usize::from(mtu);
    // Received into in place and handed over whole; the slots handed over are replaced.
    let mut slots: Vec<PacketMut> = (0..BATCH).map(|_| PacketMut::new(mtu)).collect();
    loop {
        let mut guard = fd.readable().await?;
        let lens = match guard.try_io(|fd| recv_batch(fd.get_ref(), &mut slots)) {
            Ok(Ok(lens)) => lens,
            Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(e)) => return Err(e.into()),
            // Nothing queued: readiness is cleared.
            Err(_) => continue,
        };
        // The engine closing its end reads as an empty message.
        let eof = lens.iter().position(|&n| n == 0);
        let lens = &lens[..eof.unwrap_or(lens.len())];
        if !lens.is_empty() {
            let mut batch: Vec<PacketMut> = slots
                .splice(..lens.len(), (0..lens.len()).map(|_| PacketMut::new(mtu)))
                .collect();
            for (packet, &n) in batch.iter_mut().zip(lens) {
                packet.truncate(n);
            }
            peers.send(batch)?;
        }
        if eof.is_some() {
            anyhow::bail!("the engine closed its end");
        }
    }
}

async fn to_engine(
    fd: &AsyncFd<OwnedFd>,
    mut peers: ForwardedPacketReceiver,
) -> anyhow::Result<()> {
    let mut batches = Vec::new();
    loop {
        batches.clear();
        if peers.recv_many(&mut batches, BATCH).await == 0 {
            anyhow::bail!("Tailscale stopped");
        }
        // An empty message would read as the end of the stream.
        let packets: Vec<&[u8]> = batches
            .iter()
            .flatten()
            .map(|p| p.as_ref())
            .filter(|p| !p.is_empty())
            .collect();
        for chunk in packets.chunks(BATCH) {
            send_all(fd, chunk).await?;
        }
    }
}

/// Sends every packet, waiting while the engine's receive queue is full rather than dropping,
/// which slows WireGuard down to what the engine takes.
async fn send_all(fd: &AsyncFd<OwnedFd>, mut packets: &[&[u8]]) -> io::Result<()> {
    while !packets.is_empty() {
        let mut guard = fd.writable().await?;
        match guard.try_io(|fd| send_batch(fd.get_ref(), packets)) {
            Ok(Ok(n)) => packets = &packets[n..],
            Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
            Ok(Err(e)) => return Err(e),
            // The queue is full: readiness is cleared, so wait for room.
            Err(_) => {}
        }
    }
    Ok(())
}

/// Receives the packets queued, at most one per slot, without waiting. Returns each one's length;
/// a packet longer than its slot is cut short, which the engine, bound by the MTU, never sends.
fn recv_batch(fd: &OwnedFd, slots: &mut [PacketMut]) -> io::Result<Vec<usize>> {
    let mut iovs: Vec<libc::iovec> = slots
        .iter_mut()
        .map(|slot| libc::iovec {
            iov_base: slot.as_mut().as_mut_ptr() as *mut _,
            iov_len: slot.len(),
        })
        .collect();
    let mut msgs = mmsghdrs(&mut iovs);
    // SAFETY: every message points at one iovec into its slot, which outlives the call.
    let n = unsafe {
        libc::recvmmsg(
            fd.as_raw_fd(),
            msgs.as_mut_ptr(),
            msgs.len() as _,
            libc::MSG_DONTWAIT as _,
            std::ptr::null_mut(),
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(msgs[..n as usize]
        .iter()
        .map(|m| m.msg_len as usize)
        .collect())
}

/// Sends as many of `packets` as the queue takes without waiting, one message each. Returns how
/// many went.
fn send_batch(fd: &OwnedFd, packets: &[&[u8]]) -> io::Result<usize> {
    let mut iovs: Vec<libc::iovec> = packets
        .iter()
        .map(|p| libc::iovec {
            iov_base: p.as_ptr() as *mut _,
            iov_len: p.len(),
        })
        .collect();
    let mut msgs = mmsghdrs(&mut iovs);
    // SAFETY: every message points at one iovec into a packet that outlives the call.
    let n = unsafe {
        libc::sendmmsg(
            fd.as_raw_fd(),
            msgs.as_mut_ptr(),
            msgs.len() as _,
            libc::MSG_DONTWAIT as _,
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// One message per iovec.
fn mmsghdrs(iovs: &mut [libc::iovec]) -> Vec<libc::mmsghdr> {
    iovs.iter_mut()
        .map(|iov| {
            // SAFETY: an all-zero mmsghdr is a valid, empty message.
            let mut msg: libc::mmsghdr = unsafe { std::mem::zeroed() };
            msg.msg_hdr.msg_iov = iov;
            msg.msg_hdr.msg_iovlen = 1;
            msg
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packets cross both ways intact, in order and in batches, as the engine's end sees them.
    #[test]
    fn batches_cross_the_socketpair_in_order() {
        let (ours, theirs) = socketpair().unwrap();
        let sent: Vec<Vec<u8>> = (0..200u8)
            .map(|i| vec![i; 1 + usize::from(i) * 5])
            .collect();
        let refs: Vec<&[u8]> = sent.iter().map(Vec::as_slice).collect();
        let mut n = 0;
        while n < refs.len() {
            n += send_batch(&ours, &refs[n..refs.len().min(n + BATCH)]).unwrap();
        }
        let mut got = Vec::new();
        let mut slots: Vec<PacketMut> = (0..BATCH).map(|_| PacketMut::new(1280)).collect();
        while got.len() < sent.len() {
            let lens = recv_batch(&theirs, &mut slots).unwrap_or_default();
            if lens.is_empty() {
                break;
            }
            got.extend(
                slots
                    .iter()
                    .zip(&lens)
                    .map(|(s, &n)| s.as_ref()[..n].to_vec()),
            );
        }
        assert_eq!(got, sent);
    }
}
