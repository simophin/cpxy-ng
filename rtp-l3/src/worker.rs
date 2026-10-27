//! Host socket contracts. Legacy worker entry points fail explicitly; the Linux
//! HTTP v2 runtime lives in rtp-service. Mobile needs a protected TCP provider.
#[cfg(target_os = "linux")]
use std::{fs, os::fd::FromRawFd};
use std::{
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    os::fd::AsRawFd,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
include!("worker_api.rs");
/// Mobile integration is deferred; never emit the removed v1 wire format.
pub fn require_http_integration() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported,"HTTP-controlled RTP v2 requires protected TCP control sockets and persistent UUID state; mobile/legacy worker integration is unsupported"))
}
pub fn run(
    _config: WorkerConfig,
    _resources: Resources,
    _running: Arc<AtomicBool>,
) -> io::Result<()> {
    require_http_integration()
}
pub fn run_client(
    _config: WorkerConfig,
    _resources: Resources,
    _running: Arc<AtomicBool>,
    _peer: SocketAddr,
) -> io::Result<()> {
    require_http_integration()
}
#[cfg(target_os = "linux")]
pub fn broker_socket(broker: &fs::File, port: u16) -> io::Result<UdpSocket> {
    let request = port.to_be_bytes();
    unsafe {
        if libc::send(
            broker.as_raw_fd(),
            request.as_ptr().cast(),
            2,
            libc::MSG_NOSIGNAL,
        ) != 2
        {
            return Err(io::Error::last_os_error());
        }
        let mut poll = libc::pollfd {
            fd: broker.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if libc::poll(&mut poll, 1, 1000) != 1 {
            return Err(io::Error::other("socket broker unavailable"));
        }
        let mut label = [0u8; 2];
        let mut ancillary = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: label.as_mut_ptr().cast(),
            iov_len: 2,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        // Field widths differ between glibc (usize) and musl (socklen_t).
        msg.msg_controllen = std::mem::size_of_val(&ancillary) as _;
        let received = libc::recvmsg(broker.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if received < 0 {
            return Err(io::Error::last_os_error());
        }
        // Own every delivered descriptor before validating the response.
        let mut sockets = Vec::new();
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let count =
                    ((*cmsg).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize) / 4;
                for index in 0..count {
                    let fd =
                        std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>().add(index));
                    sockets.push(UdpSocket::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
        if received != 2
            || label != request
            || msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
            || sockets.len() != 1
        {
            return Err(io::Error::other("invalid socket broker label"));
        }
        let socket = sockets.pop().unwrap();
        socket.set_nonblocking(true)?;
        Ok(socket)
    }
}
#[cfg(target_os = "linux")]
fn disconnect(socket: &UdpSocket) -> io::Result<SocketAddr> {
    let peer = socket.peer_addr()?;
    let local = socket.local_addr()?;
    let addr = libc::sockaddr {
        sa_family: libc::AF_UNSPEC as _,
        sa_data: [0; 14],
    };
    if unsafe { libc::connect(socket.as_raw_fd(), &addr, std::mem::size_of_val(&addr) as _) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Linux may clear an implicitly bound port on AF_UNSPEC. Restore the
    // original tuple, in the socket's original netns, before sending again.
    if socket.local_addr()?.port() != local.port() {
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let len = match local {
            SocketAddr::V4(local) => {
                let addr = libc::sockaddr_in {
                    sin_family: libc::AF_INET as _,
                    sin_port: local.port().to_be(),
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(local.ip().octets()),
                    },
                    sin_zero: [0; 8],
                };
                unsafe {
                    std::ptr::write(
                        (&mut storage as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in>(),
                        addr,
                    )
                };
                std::mem::size_of::<libc::sockaddr_in>()
            }
            SocketAddr::V6(local) => {
                let addr = libc::sockaddr_in6 {
                    sin6_family: libc::AF_INET6 as _,
                    sin6_port: local.port().to_be(),
                    sin6_flowinfo: local.flowinfo().to_be(),
                    sin6_addr: libc::in6_addr {
                        s6_addr: local.ip().octets(),
                    },
                    sin6_scope_id: local.scope_id(),
                };
                unsafe {
                    std::ptr::write(
                        (&mut storage as *mut libc::sockaddr_storage).cast::<libc::sockaddr_in6>(),
                        addr,
                    )
                };
                std::mem::size_of::<libc::sockaddr_in6>()
            }
        };
        if unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&storage as *const libc::sockaddr_storage).cast(),
                len as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    if socket.local_addr()?.port() != local.port() {
        return Err(io::Error::other("disconnect changed local port"));
    }
    Ok(peer)
}
