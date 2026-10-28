//! Bounded UDP batching experiment. Not selected by production packet workers yet.
//!
//! The caller retains lifecycle/replay ownership. Receive buffers and Linux ABI
//! descriptors are allocated once. Sends borrow already-encrypted datagrams;
//! retiring only the returned prefix preserves the exact ciphertext on retry.
use super::codec::MAX_FRAME;
use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

pub const MAX_BATCH: usize = 64;

#[derive(Clone, Copy, Debug)]
pub enum Backend {
    Portable,
    #[cfg(target_os = "linux")]
    Mmsg,
}

pub struct Received {
    wire: [u8; MAX_FRAME + 1],
    len: usize,
    pub peer: Option<SocketAddr>,
    pub truncated: bool,
    pub control_truncated: bool,
    /// SO_RXQ_OVFL's cumulative u32 count for this socket (may wrap).
    pub socket_drops: Option<u32>,
}
impl Received {
    /// Oversized/truncated messages must not reach authentication or path state.
    pub fn frame(&self) -> Option<&[u8]> {
        (!self.truncated && self.len <= MAX_FRAME).then(|| &self.wire[..self.len])
    }
}

pub struct ReceiveBatch {
    backend: Backend,
    packets: Box<[Received]>,
    #[cfg(target_os = "linux")]
    linux: linux::Descriptors,
}
impl ReceiveBatch {
    pub fn new(backend: Backend, budget: usize) -> io::Result<Self> {
        if !(1..=MAX_BATCH).contains(&budget) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP batch budget must be 1..64",
            ));
        }
        Ok(Self {
            backend,
            packets: (0..budget)
                .map(|_| Received {
                    wire: [0; MAX_FRAME + 1],
                    len: 0,
                    peer: None,
                    truncated: false,
                    control_truncated: false,
                    socket_drops: None,
                })
                .collect(),
            #[cfg(target_os = "linux")]
            linux: linux::Descriptors::new(budget),
        })
    }
    /// Never waits for a full batch. Sockets must be nonblocking on portable
    /// targets; Linux also uses MSG_DONTWAIT. Empty readiness returns WouldBlock.
    pub fn recv(&mut self, socket: &UdpSocket) -> io::Result<&[Received]> {
        let count = match self.backend {
            Backend::Portable => {
                let mut count = 0;
                for p in &mut self.packets {
                    match socket.recv_from(&mut p.wire) {
                        Ok((len, peer)) => {
                            p.len = len;
                            p.peer = Some(peer);
                            // The extra byte detects every oversized protocol
                            // frame even where recv_from lacks MSG_TRUNC.
                            p.truncated = len > MAX_FRAME;
                            p.control_truncated = false;
                            p.socket_drops = None;
                            count += 1;
                        }
                        Err(e)
                            if count > 0
                                && matches!(
                                    e.kind(),
                                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                                ) =>
                        {
                            break
                        }
                        Err(e) if count == 0 => return Err(e),
                        // Preserve successfully received messages even if a
                        // later datagram reports a socket error.
                        Err(_) => break,
                    }
                }
                count
            }
            #[cfg(target_os = "linux")]
            Backend::Mmsg => self.linux.recv(socket, &mut self.packets)?,
        };
        Ok(&self.packets[..count])
    }
}

pub struct Datagram<'a> {
    pub frame: &'a [u8],
    pub peer: SocketAddr,
}

pub struct SendBatch {
    backend: Backend,
    budget: usize,
    #[cfg(target_os = "linux")]
    linux: linux::Descriptors,
}
impl SendBatch {
    pub fn new(backend: Backend, budget: usize) -> io::Result<Self> {
        if !(1..=MAX_BATCH).contains(&budget) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP batch budget must be 1..64",
            ));
        }
        Ok(Self {
            backend,
            budget,
            #[cfg(target_os = "linux")]
            linux: linux::Descriptors::new(budget),
        })
    }
    /// Returns only the successfully sent prefix. The caller must retain the
    /// suffix byte-for-byte, without reserving replacement packet numbers.
    pub fn send(&mut self, socket: &UdpSocket, packets: &[Datagram<'_>]) -> io::Result<usize> {
        let packets = &packets[..packets.len().min(self.budget)];
        if packets.is_empty() {
            return Ok(0);
        }
        if packets.iter().any(|p| p.frame.len() > MAX_FRAME) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "oversized RTP frame",
            ));
        }
        match self.backend {
            Backend::Portable => {
                let mut count = 0;
                for p in packets {
                    match socket.send_to(p.frame, p.peer) {
                        Ok(n) if n == p.frame.len() => count += 1,
                        Ok(_) if count == 0 => {
                            return Err(io::Error::other("partial UDP datagram"))
                        }
                        Err(e) if count == 0 => return Err(e),
                        _ => break,
                    }
                }
                Ok(count)
            }
            #[cfg(target_os = "linux")]
            Backend::Mmsg => self.linux.send(socket, packets),
        }
    }
}

/// Opt in to per-socket receive overflow metadata before collecting packets.
/// Other platforms retain the adapter without this Linux-only diagnostic.
pub fn enable_drop_counter(socket: &UdpSocket) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let enabled: libc::c_int = 1;
        // SAFETY: valid descriptor and an initialized integer of the supplied length.
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RXQ_OVFL,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as _,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = socket;
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{mem, os::fd::AsRawFd, ptr};

    // c_ulong provides cmsghdr alignment on both 32- and 64-bit Linux ABIs.
    type Control = [libc::c_ulong; 8];
    pub(super) struct Descriptors {
        headers: Box<[libc::mmsghdr]>,
        vectors: Box<[libc::iovec]>,
        addresses: Box<[libc::sockaddr_storage]>,
        controls: Box<[Control]>,
    }
    // SAFETY: these exclusively owned arrays contain raw pointers only during
    // synchronous syscalls. Every pointer is rebound before use after a move;
    // neither dropping nor moving a descriptor dereferences its old pointers.
    unsafe impl Send for Descriptors {}

    impl Descriptors {
        pub(super) fn new(budget: usize) -> Self {
            Self {
                // SAFETY: all-zero C ABI descriptor/storage types have valid bit patterns.
                headers: (0..budget).map(|_| unsafe { mem::zeroed() }).collect(),
                vectors: (0..budget).map(|_| unsafe { mem::zeroed() }).collect(),
                addresses: (0..budget).map(|_| unsafe { mem::zeroed() }).collect(),
                controls: vec![[0; 8]; budget].into_boxed_slice(),
            }
        }
        pub(super) fn recv(
            &mut self,
            socket: &UdpSocket,
            packets: &mut [Received],
        ) -> io::Result<usize> {
            for (i, p) in packets.iter_mut().enumerate() {
                self.vectors[i] = libc::iovec {
                    iov_base: p.wire.as_mut_ptr().cast(),
                    iov_len: p.wire.len(),
                };
                // Zero libc-private musl padding as well as the public fields.
                let mut header: libc::mmsghdr = unsafe { mem::zeroed() };
                header.msg_hdr.msg_name =
                    (&mut self.addresses[i] as *mut libc::sockaddr_storage).cast();
                header.msg_hdr.msg_namelen = mem::size_of::<libc::sockaddr_storage>() as _;
                header.msg_hdr.msg_iov = &mut self.vectors[i];
                header.msg_hdr.msg_iovlen = 1;
                header.msg_hdr.msg_control = self.controls[i].as_mut_ptr().cast();
                header.msg_hdr.msg_controllen = mem::size_of::<Control>() as _;
                self.headers[i] = header;
            }
            // SAFETY: all headers point to separate initialized buffers owned by
            // this call. MSG_DONTWAIT avoids recvmmsg's blocking timeout semantics.
            let n = unsafe {
                libc::recvmmsg(
                    socket.as_raw_fd(),
                    self.headers.as_mut_ptr(),
                    packets.len() as _,
                    (libc::MSG_DONTWAIT | libc::MSG_TRUNC) as _,
                    ptr::null_mut(),
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            for (i, p) in packets[..n as usize].iter_mut().enumerate() {
                let h = &self.headers[i];
                p.len = (h.msg_len as usize).min(p.wire.len());
                p.truncated =
                    h.msg_hdr.msg_flags & libc::MSG_TRUNC != 0 || h.msg_len as usize > MAX_FRAME;
                p.control_truncated = h.msg_hdr.msg_flags & libc::MSG_CTRUNC != 0;
                p.peer = decode_address(&self.addresses[i], h.msg_hdr.msg_namelen as usize);
                p.socket_drops = drops(&h.msg_hdr);
            }
            Ok(n as usize)
        }
        pub(super) fn send(
            &mut self,
            socket: &UdpSocket,
            packets: &[Datagram<'_>],
        ) -> io::Result<usize> {
            for (i, p) in packets.iter().enumerate() {
                let address_len = encode_address(p.peer, &mut self.addresses[i]);
                self.vectors[i] = libc::iovec {
                    iov_base: p.frame.as_ptr().cast_mut().cast(),
                    iov_len: p.frame.len(),
                };
                let mut header: libc::mmsghdr = unsafe { mem::zeroed() };
                header.msg_hdr.msg_name =
                    (&mut self.addresses[i] as *mut libc::sockaddr_storage).cast();
                header.msg_hdr.msg_namelen = address_len as _;
                header.msg_hdr.msg_iov = &mut self.vectors[i];
                header.msg_hdr.msg_iovlen = 1;
                self.headers[i] = header;
            }
            // SAFETY: all borrowed ciphertext and address buffers remain alive
            // through the synchronous call; sendmmsg never modifies iovec data.
            let n = unsafe {
                libc::sendmmsg(
                    socket.as_raw_fd(),
                    self.headers.as_mut_ptr(),
                    packets.len() as _,
                    (libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) as _,
                )
            };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }
    }
    fn decode_address(storage: &libc::sockaddr_storage, len: usize) -> Option<SocketAddr> {
        // SAFETY: sockaddr_storage is sufficiently sized/aligned for both types;
        // family and returned length are checked before reading either layout.
        unsafe {
            match storage.ss_family as libc::c_int {
                libc::AF_INET if len >= mem::size_of::<libc::sockaddr_in>() => {
                    let a = &*(storage as *const _ as *const libc::sockaddr_in);
                    Some(SocketAddr::from((
                        a.sin_addr.s_addr.to_ne_bytes(),
                        u16::from_be(a.sin_port),
                    )))
                }
                libc::AF_INET6 if len >= mem::size_of::<libc::sockaddr_in6>() => {
                    let a = &*(storage as *const _ as *const libc::sockaddr_in6);
                    Some(SocketAddr::V6(std::net::SocketAddrV6::new(
                        a.sin6_addr.s6_addr.into(),
                        u16::from_be(a.sin6_port),
                        u32::from_be(a.sin6_flowinfo),
                        a.sin6_scope_id,
                    )))
                }
                _ => None,
            }
        }
    }
    fn encode_address(peer: SocketAddr, storage: &mut libc::sockaddr_storage) -> usize {
        // SAFETY: storage has sufficient alignment/size; write initializes the
        // complete sockaddr layout before its address and exact size are used.
        unsafe {
            *storage = mem::zeroed();
            match peer {
                SocketAddr::V4(a) => {
                    let mut raw: libc::sockaddr_in = mem::zeroed();
                    raw.sin_family = libc::AF_INET as _;
                    raw.sin_port = a.port().to_be();
                    raw.sin_addr.s_addr = u32::from_ne_bytes(a.ip().octets());
                    ptr::write((storage as *mut libc::sockaddr_storage).cast(), raw);
                    mem::size_of_val(&raw)
                }
                SocketAddr::V6(a) => {
                    let raw = libc::sockaddr_in6 {
                        sin6_family: libc::AF_INET6 as _,
                        sin6_port: a.port().to_be(),
                        sin6_flowinfo: a.flowinfo().to_be(),
                        sin6_scope_id: a.scope_id(),
                        sin6_addr: libc::in6_addr {
                            s6_addr: a.ip().octets(),
                        },
                    };
                    ptr::write((storage as *mut libc::sockaddr_storage).cast(), raw);
                    mem::size_of_val(&raw)
                }
            }
        }
    }
    fn drops(header: &libc::msghdr) -> Option<u32> {
        // SAFETY: called only with a kernel-filled header referencing our aligned
        // ancillary buffer. CMSG_NXTHDR respects msg_controllen. read_unaligned
        // does not assume alignment of CMSG_DATA on every libc ABI.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(header);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET
                    && (*cmsg).cmsg_type == libc::SO_RXQ_OVFL
                    && (*cmsg).cmsg_len as usize
                        >= libc::CMSG_LEN(mem::size_of::<u32>() as _) as usize
                {
                    return Some(ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<u32>()));
                }
                cmsg = libc::CMSG_NXTHDR(header, cmsg);
            }
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn ancillary_and_scoped_ipv6_layouts() {
            let mut storage = unsafe { mem::zeroed() };
            for peer in ["127.0.0.1:1234", "[fe80::1%7]:5432"] {
                let peer = peer.parse().unwrap();
                let len = encode_address(peer, &mut storage);
                assert_eq!(decode_address(&storage, len), Some(peer));
                assert_eq!(decode_address(&storage, 0), None);
            }
            let mut control: Control = [0; 8];
            let mut h: libc::msghdr = unsafe { mem::zeroed() };
            h.msg_control = control.as_mut_ptr().cast();
            h.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&h);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SO_RXQ_OVFL;
                (*cmsg).cmsg_len = libc::CMSG_LEN(4) as _;
                ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<u32>(), u32::MAX);
                assert_eq!(drops(&h), Some(u32::MAX));
                (*cmsg).cmsg_len = libc::CMSG_LEN(0) as _;
                assert_eq!(drops(&h), None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::codec::{Control, Direction, Packet, Rx, Tx};
    use std::{
        net::Ipv4Addr,
        time::{Duration, Instant},
    };

    fn socket(ip: &str) -> UdpSocket {
        let s = UdpSocket::bind(format!("{ip}:0")).unwrap();
        s.set_nonblocking(true).unwrap();
        s
    }
    fn receive<'a>(batch: &'a mut ReceiveBatch, socket: &UdpSocket) -> &'a [Received] {
        // Wait for kernel readability outside the adapter; it must never wait
        // for a batch to fill or introduce a packet coalescing timer.
        use std::os::fd::AsRawFd;
        let mut fd = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 1000) }, 1);
        batch.recv(socket).unwrap()
    }
    fn backends() -> Vec<Backend> {
        let mut b = vec![Backend::Portable];
        #[cfg(target_os = "linux")]
        b.push(Backend::Mmsg);
        b
    }
    #[test]
    fn batch_boundaries_sources_and_truncation_across_backends() {
        for backend in backends() {
            for ip in ["127.0.0.1", "[::1]"] {
                let receiver = socket(ip);
                let sender = socket(ip);
                enable_drop_counter(&receiver).unwrap();
                let peer = receiver.local_addr().unwrap();
                let bodies = [
                    vec![],
                    vec![1; MAX_FRAME],
                    vec![2; MAX_FRAME + 100],
                    vec![3],
                ];
                // Use ordinary send_to for an oversized frame the adapter refuses.
                for body in &bodies {
                    sender.send_to(body, peer).unwrap();
                }
                let mut batch = ReceiveBatch::new(backend, 3).unwrap();
                let got = receive(&mut batch, &receiver);
                assert_eq!(got.len(), 3);
                assert_eq!(got[0].frame(), Some(&[][..]));
                assert_eq!(got[1].frame(), Some(bodies[1].as_slice()));
                assert_eq!(got[2].frame(), None);
                assert!(got[2].truncated);
                for p in got {
                    assert_eq!(p.peer, Some(sender.local_addr().unwrap()));
                }
                let got = receive(&mut batch, &receiver);
                assert_eq!(got.len(), 1);
                assert_eq!(got[0].frame(), Some(&[3][..]));
                assert!(!got[0].truncated);
                let before = Instant::now();
                assert_eq!(
                    batch.recv(&receiver).err().unwrap().kind(),
                    io::ErrorKind::WouldBlock
                );
                assert!(before.elapsed() < Duration::from_millis(100));
            }
        }
    }
    #[test]
    fn authenticated_frames_tampering_and_replay_unchanged() {
        let inner = Ipv4Addr::new(10, 88, 0, 2);
        for backend in backends() {
            let sender = socket("127.0.0.1");
            let receiver = socket("127.0.0.1");
            let mut tx = Tx::new(&[1; 32], [2; 16], Direction::Upstream, inner);
            let mut rx = Rx::new(&[1; 32], [2; 16], Direction::Upstream, inner);
            let valid = tx.control(Control::Ping([3; 16]), 0).unwrap();
            let mut tampered = valid.clone();
            *tampered.last_mut().unwrap() ^= 1;
            let bodies = [tampered, valid.clone(), valid];
            let packets: Vec<_> = bodies
                .iter()
                .map(|frame| Datagram {
                    frame,
                    peer: receiver.local_addr().unwrap(),
                })
                .collect();
            let mut send = SendBatch::new(backend, 32).unwrap();
            assert_eq!(send.send(&sender, &packets).unwrap(), 3);
            let mut batch = ReceiveBatch::new(backend, 32).unwrap();
            let got = receive(&mut batch, &receiver);
            assert_eq!(got.len(), 3);
            assert!(rx.open(got[0].frame().unwrap()).is_none());
            assert!(matches!(
                rx.open(got[1].frame().unwrap()),
                Some(Packet::Control(Control::Ping(_)))
            ));
            assert!(rx.open(got[2].frame().unwrap()).is_none());
        }
    }
    #[test]
    fn budgets_validate_and_bound_storage_and_sends() {
        for backend in backends() {
            for size in [0, 65, usize::MAX] {
                assert!(ReceiveBatch::new(backend, size).is_err());
                assert!(SendBatch::new(backend, size).is_err());
            }
            for budget in [16, 32, 64] {
                let sender = socket("127.0.0.1");
                let receiver = socket("127.0.0.1");
                let packets: Vec<_> = (0..65)
                    .map(|_| Datagram {
                        frame: &[4],
                        peer: receiver.local_addr().unwrap(),
                    })
                    .collect();
                let mut send = SendBatch::new(backend, budget).unwrap();
                assert_eq!(send.send(&sender, &packets).unwrap(), budget);
                let mut recv = ReceiveBatch::new(backend, budget).unwrap();
                assert_eq!(receive(&mut recv, &receiver).len(), budget);
                assert_eq!(recv.packets.len(), budget);
            }
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn partial_send_retries_identical_ciphertext_suffix() {
        // A zero destination port gives sendmmsg a deterministic error after
        // one success, exercising its successful-prefix result on real sockets.
        let sender = socket("127.0.0.1");
        let receiver = socket("127.0.0.1");
        let peer = receiver.local_addr().unwrap();
        let inner = Ipv4Addr::new(10, 88, 0, 2);
        let mut tx = Tx::new(&[1; 32], [2; 16], Direction::Upstream, inner);
        let mut rx = Rx::new(&[1; 32], [2; 16], Direction::Upstream, inner);
        let first = tx.control(Control::Ping([1; 16]), 0).unwrap();
        let suffix = tx.control(Control::Ping([2; 16]), 0).unwrap();
        let mut send = SendBatch::new(Backend::Mmsg, 32).unwrap();
        assert_eq!(
            send.send(
                &sender,
                &[
                    Datagram {
                        frame: &first,
                        peer
                    },
                    Datagram {
                        frame: &suffix,
                        peer: "127.0.0.1:0".parse().unwrap()
                    },
                ]
            )
            .unwrap(),
            1
        );
        assert_eq!(
            send.send(
                &sender,
                &[Datagram {
                    frame: &suffix,
                    peer
                }]
            )
            .unwrap(),
            1
        );
        let mut batch = ReceiveBatch::new(Backend::Mmsg, 32).unwrap();
        let got = receive(&mut batch, &receiver);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].frame(), Some(first.as_slice()));
        assert_eq!(got[1].frame(), Some(suffix.as_slice()));
        assert!(rx.open(got[0].frame().unwrap()).is_some());
        assert!(rx.open(got[1].frame().unwrap()).is_some());
        assert!(rx.open(&suffix).is_none());
    }
}
