//! Private descriptor broker. Only a configured IP peer, bind and device cross
//! into this policy; tunnel packets, keys, routes and firewall are not involved.
use crate::config::{Role, Settings};
use std::{
    io,
    net::{SocketAddr, TcpStream, UdpSocket},
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    time::Duration,
};

pub struct Policy {
    entry: SocketAddr,
    bind: SocketAddr,
    device: String,
}
impl Policy {
    pub fn from_settings(settings: &Settings) -> io::Result<Self> {
        settings.validate()?;
        let Role::Client { entry, bind } = settings.transport else {
            return Err(io::Error::other("namespace-run requires client profile"));
        };
        Ok(Self {
            entry,
            bind,
            device: settings.outbound_interface.clone().unwrap(),
        })
    }
    fn socket(&self, port: u16) -> io::Result<UdpSocket> {
        if port == 0 {
            return Err(io::Error::other("zero broker port"));
        }
        let socket = UdpSocket::bind(self.bind)?;
        let device = std::ffi::CString::new(self.device.as_str()).map_err(io::Error::other)?;
        if unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                device.as_ptr().cast(),
                device.as_bytes_with_nul().len() as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut peer = self.entry;
        peer.set_port(port);
        socket.connect(peer)?;
        Ok(socket)
    }
    /// Serve at most one request, with bounded waiting so child exit/signals can
    /// always be observed by the supervisor. Malformed requests fail closed.
    pub fn serve_once(&self, channel: &UnixDatagram) -> io::Result<bool> {
        channel.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut request = [0u8; 4];
        let length = match channel.recv(&mut request) {
            Ok(length) => length,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(false)
            }
            Err(e) => return Err(e),
        };
        if length != 2 && !(length == 3 && request[0] == 1) {
            return Err(io::Error::other("invalid socket broker request"));
        }
        let offset = length - 2;
        let port = u16::from_be_bytes([request[offset], request[offset + 1]]);
        if port == 0 {
            return Err(io::Error::other("zero broker port"));
        }
        // Allocation failure is local to this candidate. A labeled no-FD reply
        // makes the worker reject it while retaining its existing working path.
        if length == 2 {
            let socket = self.socket(port).ok();
            send_socket(channel, &request[..2], socket.as_ref())?;
        } else {
            let mut peer = self.entry;
            peer.set_port(port);
            // TCP is restricted to the configured control port and host device.
            let socket = if port == self.entry.port() {
                tcp_socket(peer, self.bind, &self.device).ok()
            } else {
                None
            };
            send_socket(channel, &request[1..3], socket.as_ref())?;
        }
        Ok(true)
    }
}
fn send_socket<T: AsRawFd>(
    channel: &UnixDatagram,
    label: &[u8],
    socket: Option<&T>,
) -> io::Result<()> {
    unsafe {
        let mut ancillary = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: label.as_ptr().cast_mut().cast(),
            iov_len: 2,
        };
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        if let Some(socket) = socket {
            message.msg_control = ancillary.as_mut_ptr().cast();
            message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<i32>() as _) as _;
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as _) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<i32>(), socket.as_raw_fd());
        }
        if libc::sendmsg(
            channel.as_raw_fd(),
            &message,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        ) != 2
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Bind the physical device before connect, including the HTTP recovery path.
pub fn tcp_socket(peer: SocketAddr, bind: SocketAddr, device: &str) -> io::Result<TcpStream> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(peer),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    let device = std::ffi::CString::new(device).map_err(io::Error::other)?;
    if unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            device.as_ptr().cast(),
            device.as_bytes_with_nul().len() as _,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut bind = bind;
    bind.set_port(0);
    socket.bind(&bind.into())?;
    socket.connect_timeout(&peer.into(), Duration::from_secs(3))?;
    Ok(socket.into())
}
/// Receive one labeled TCP descriptor from the existing private host broker.
pub fn tcp_from_broker(channel: &std::fs::File, port: u16) -> io::Result<TcpStream> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let request = [1, (port >> 8) as u8, port as u8];
    unsafe {
        if libc::send(
            channel.as_raw_fd(),
            request.as_ptr().cast(),
            3,
            libc::MSG_NOSIGNAL,
        ) != 3
        {
            return Err(io::Error::last_os_error());
        }
        let mut poll = libc::pollfd {
            fd: channel.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if libc::poll(&mut poll, 1, 4000) != 1 {
            return Err(io::Error::other("HTTP socket broker unavailable"));
        }
        let mut label = [0; 2];
        let mut ancillary = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: label.as_mut_ptr().cast(),
            iov_len: 2,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&ancillary) as _;
        let n = libc::recvmsg(channel.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut fds = Vec::new();
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let count = ((*c).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize)
                    / std::mem::size_of::<i32>();
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
                        libc::CMSG_DATA(c).cast::<i32>().add(i),
                    )));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
        if n != 2
            || label != request[1..]
            || msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
            || fds.len() != 1
        {
            return Err(io::Error::other("invalid HTTP socket broker response"));
        }
        Ok(TcpStream::from(fds.pop().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        os::fd::{FromRawFd, IntoRawFd},
        thread,
    };
    fn policy() -> Policy {
        Policy {
            entry: "127.0.0.1:1".parse().unwrap(),
            bind: "127.0.0.1:0".parse().unwrap(),
            device: "lo".into(),
        }
    }
    #[test]
    fn replacement_sockets_are_host_bound_connected_and_labeled() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();
        let (client, parent) = UnixDatagram::pair().unwrap();
        let task = thread::spawn(move || {
            for _ in 0..2 {
                assert!(policy().serve_once(&parent).unwrap());
            }
        });
        let client = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        let a = rtp_l3::linux::broker_socket(&client, port).unwrap();
        let b = rtp_l3::linux::broker_socket(&client, port).unwrap();
        assert_ne!(a.local_addr().unwrap(), b.local_addr().unwrap());
        for socket in [a, b] {
            assert_eq!(socket.peer_addr().unwrap(), receiver.local_addr().unwrap());
            socket.send(b"probe").unwrap();
            let mut buffer = [0; 5];
            assert_eq!(
                receiver.recv_from(&mut buffer).unwrap().1,
                socket.local_addr().unwrap()
            );
        }
        task.join().unwrap();
    }
    #[test]
    fn allocation_failure_rejects_candidate_and_broker_keeps_serving() {
        let occupied = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut policy = policy();
        policy.bind = occupied.local_addr().unwrap();
        let (client, parent) = UnixDatagram::pair().unwrap();
        let task = thread::spawn(move || {
            assert!(policy.serve_once(&parent).unwrap());
            drop(occupied);
            assert!(policy.serve_once(&parent).unwrap());
        });
        let client = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        assert!(rtp_l3::linux::broker_socket(&client, 4001).is_err());
        assert!(rtp_l3::linux::broker_socket(&client, 4001).is_ok());
        task.join().unwrap();
    }
    #[test]
    fn replacement_ipv6_socket_keeps_the_protected_host_peer() {
        let receiver = UdpSocket::bind("[::1]:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let port = receiver.local_addr().unwrap().port();
        let (client, parent) = UnixDatagram::pair().unwrap();
        let task = thread::spawn(move || {
            let policy = Policy {
                entry: "[::1]:1".parse().unwrap(),
                bind: "[::]:0".parse().unwrap(),
                device: "lo".into(),
            };
            for _ in 0..2 {
                assert!(policy.serve_once(&parent).unwrap());
            }
        });
        let client = unsafe { File::from_raw_fd(client.into_raw_fd()) };
        for _ in 0..2 {
            let socket = rtp_l3::linux::broker_socket(&client, port).unwrap();
            assert_eq!(socket.peer_addr().unwrap(), receiver.local_addr().unwrap());
            socket.send(b"v6").unwrap();
            let mut buffer = [0; 2];
            assert_eq!(receiver.recv_from(&mut buffer).unwrap().0, 2);
        }
        task.join().unwrap();
    }
    #[test]
    fn rejects_malformed_and_zero_port_requests() {
        for request in [&[0, 0][..], &[1][..], &[2, 2, 3][..], &[1, 2, 3, 4][..]] {
            let (client, parent) = UnixDatagram::pair().unwrap();
            client.send(request).unwrap();
            assert!(policy().serve_once(&parent).is_err());
        }
    }
}
