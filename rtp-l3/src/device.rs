//! The packet device as a pair of bounded channels, so a worker never touches the TUN.
//! Platforms with their own packet handling (the mobile shim) hold the [`Port`]; a plain
//! descriptor is attached with [`pump`].
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
};
use tokio::{io::unix::AsyncFd, sync::mpsc};

/// Packets queued toward the device: absorbs a burst while the device side is briefly
/// descheduled, and beyond it the worker drops and counts. Every queued packet is latency
/// (256 is about 20 ms at 128 Mbit/s); a 1024-packet queue measurably raised loaded RTT.
pub const WRITE_QUEUE: usize = 256;
/// Packets read from the device and not yet taken by the worker.
pub const READ_QUEUE: usize = 256;

/// The worker's side: IP packets read from the device, and the queue of packets to write.
/// Either channel closing means the device is gone.
pub struct Device {
    pub reads: mpsc::Receiver<Vec<u8>>,
    pub writes: mpsc::Sender<Vec<u8>>,
}

/// The device's side of a [`Device`].
pub struct Port {
    pub reads: mpsc::Sender<Vec<u8>>,
    pub writes: mpsc::Receiver<Vec<u8>>,
}

pub fn channel() -> (Device, Port) {
    let (read_tx, read_rx) = mpsc::channel(READ_QUEUE);
    let (write_tx, write_rx) = mpsc::channel(WRITE_QUEUE);
    (
        Device {
            reads: read_rx,
            writes: write_tx,
        },
        Port {
            reads: read_tx,
            writes: write_rx,
        },
    )
}

pub fn set_nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: plain fcntl on a descriptor the caller owns.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Moves packets between a packet-preserving descriptor (a TUN, or a datagram socket in
/// tests) and a worker until the worker drops its [`Device`]. `accept` filters packets
/// before they are written. Must run on a Tokio runtime with I/O enabled.
pub async fn pump(fd: OwnedFd, port: Port, accept: impl Fn(&[u8]) -> bool) -> io::Result<()> {
    set_nonblocking(&fd)?;
    // SAFETY: the AsyncFd owns the descriptor, so it stays open while registered.
    let fd = unsafe { AsyncFd::register(fd) }.map_err(io::Error::from)?;
    let Port { reads, mut writes } = port;
    let read = async {
        let mut buf = vec![0u8; 65536];
        loop {
            let mut guard = fd.readable().await?;
            match guard.try_io(|fd| {
                // SAFETY: reads into a buffer of the given length.
                let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(0)) | Err(_) => {}
                Ok(Ok(n)) => {
                    if reads.send(buf[..n].to_vec()).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Err(e)) => return Err(e),
            }
        }
    };
    let write = async {
        // Owned by this half, so the filter need only be Send.
        let accept = accept;
        while let Some(packet) = writes.recv().await {
            if !accept(&packet) {
                continue;
            }
            loop {
                let mut guard = fd.writable().await?;
                match guard.try_io(|fd| {
                    // SAFETY: writes from a buffer of the given length.
                    let n = unsafe {
                        libc::write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len())
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                }) {
                    Ok(Ok(n)) if n == packet.len() => break,
                    Ok(Ok(_)) => return Err(io::Error::other("partial packet write")),
                    Ok(Err(e)) => return Err(e),
                    Err(_would_block) => {}
                }
            }
        }
        Ok(())
    };
    tokio::select! {
        result = read => result,
        result = write => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;

    #[tokio::test]
    async fn pump_preserves_packets_both_ways_filters_writes_and_ends_with_the_worker() {
        let (outside, inside) = UnixDatagram::pair().unwrap();
        outside.set_nonblocking(true).unwrap();
        let outside = tokio::net::UnixDatagram::from_std(outside).unwrap();
        let (mut device, port) = channel();
        let pump = tokio::spawn(pump(inside.into(), port, |p: &[u8]| p != b"refused"));
        outside.send(b"first").await.unwrap();
        outside.send(b"second").await.unwrap();
        assert_eq!(device.reads.recv().await.unwrap(), b"first");
        assert_eq!(device.reads.recv().await.unwrap(), b"second");
        for packet in [&b"refused"[..], b"to device"] {
            device.writes.send(packet.to_vec()).await.unwrap();
        }
        let mut buf = [0; 64];
        let n = outside.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"to device");
        drop(device);
        pump.await.unwrap().unwrap();
    }
}
