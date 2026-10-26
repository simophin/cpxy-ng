//! Sits between the TUN and the `rtp-l3` worker, which reads and writes the other end of a
//! datagram socketpair: answers DNS, refuses what the tunnel cannot carry, and injects the
//! engine's own DNS queries into the tunnel.

use crate::dns::TunnelQueries;
use crate::packet::{self, Incoming, Outgoing};
use client::dns_split::server::{DnsSplitHandler, encode_udp};
use hickory_proto::op::Message;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

/// Packets through the shim, in both directions.
#[derive(Default)]
pub struct Counters {
    pub sent_packets: AtomicU64,
    pub sent_bytes: AtomicU64,
    pub received_packets: AtomicU64,
    pub received_bytes: AtomicU64,
    pub dns_queries: AtomicU64,
    pub refused: AtomicU64,
}

/// One packet per read or write on a nonblocking descriptor: a TUN or a datagram socket.
#[derive(Clone)]
pub struct Datagrams {
    fd: Arc<AsyncFd<OwnedFd>>,
    /// The utun packet-information header (iOS, macOS): a 4-byte address family.
    family_header: bool,
}

impl Datagrams {
    /// Must be called on the runtime.
    pub fn new(fd: OwnedFd, family_header: bool) -> io::Result<Self> {
        set_nonblocking(&fd)?;
        // SAFETY: the AsyncFd owns the descriptor, so it stays open while registered.
        let fd = unsafe {
            AsyncFd::register_with_interest(fd, Interest::READABLE | Interest::WRITABLE)?
        };
        Ok(Self {
            fd: Arc::new(fd),
            family_header,
        })
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            let n = match guard.try_io(|fd| {
                // SAFETY: reads into a buffer of the given length.
                let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => result?,
                Err(_would_block) => continue,
            };
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if !self.family_header {
                return Ok(n);
            }
            if n <= 4 {
                continue;
            }
            buf.copy_within(4..n, 0);
            return Ok(n - 4);
        }
    }

    pub async fn send(&self, packet: &[u8]) -> io::Result<()> {
        let framed;
        let packet = if self.family_header {
            let family = if packet.first().is_some_and(|b| b >> 4 == 6) {
                libc::AF_INET6
            } else {
                libc::AF_INET
            };
            framed = [&(family as u32).to_be_bytes()[..], packet].concat();
            &framed[..]
        } else {
            packet
        };
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|fd| {
                // SAFETY: writes from a buffer of the given length.
                let n =
                    unsafe { libc::write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) if n == packet.len() => return Ok(()),
                Ok(Ok(_)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

pub fn set_nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: plain fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub struct Shim {
    pub tun: Datagrams,
    pub tunnel: Datagrams,
    pub own: Ipv4Addr,
    pub dns: Ipv4Addr,
    pub resolvers: Vec<Ipv4Addr>,
    pub handler: Arc<DnsSplitHandler>,
    pub queries: Arc<TunnelQueries>,
    pub counters: Arc<Counters>,
}

impl Shim {
    /// From the device into the tunnel. Ends when the TUN fails or closes.
    pub async fn outbound(self: Arc<Self>) {
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match self.tun.recv(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("Reading the TUN failed: {e}");
                    return;
                }
            };
            let packet = &buf[..n];
            match packet::outgoing(packet, self.own, self.dns) {
                Outgoing::Tunnel => {
                    if self.tunnel.send(packet).await.is_ok() {
                        self.counters.sent_packets.fetch_add(1, Ordering::Relaxed);
                        self.counters
                            .sent_bytes
                            .fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
                Outgoing::Dns { src, port, payload } => {
                    self.counters.dns_queries.fetch_add(1, Ordering::Relaxed);
                    match Message::from_vec(&packet[payload]) {
                        Ok(request) => {
                            tokio::spawn(self.clone().answer(request, src, port));
                        }
                        Err(e) => tracing::debug!("Invalid DNS request: {e}"),
                    }
                }
                Outgoing::Reply(reply) => {
                    self.counters.refused.fetch_add(1, Ordering::Relaxed);
                    let _ = self.tun.send(&reply).await;
                }
                Outgoing::Drop => {}
            }
        }
    }

    /// From the tunnel to the device. Ends when the worker's end of the socketpair closes.
    pub async fn inbound(self: Arc<Self>) {
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match self.tunnel.recv(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("Reading from the tunnel failed: {e}");
                    return;
                }
            };
            let packet = &buf[..n];
            match packet::incoming(packet, self.own, &self.resolvers) {
                Incoming::Tun => {
                    if self.tun.send(packet).await.is_ok() {
                        self.counters
                            .received_packets
                            .fetch_add(1, Ordering::Relaxed);
                        self.counters
                            .received_bytes
                            .fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
                Incoming::DnsReply { port, payload } => {
                    self.queries.deliver(port, &packet[payload]);
                }
                Incoming::Drop => {}
            }
        }
    }

    async fn answer(self: Arc<Self>, request: Message, to: Ipv4Addr, port: u16) {
        let response = self.handler.handle(&request).await;
        match encode_udp(&request, &response) {
            Ok(payload) => {
                let reply = packet::udp_v4(self.dns, 53, to, port, &payload);
                if let Err(e) = self.tun.send(&reply).await {
                    tracing::debug!("Writing a DNS reply failed: {e}");
                }
            }
            Err(e) => tracing::warn!("{e:?}"),
        }
    }
}
