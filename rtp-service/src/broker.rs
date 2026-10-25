//! Private descriptor broker. Only a configured IPv4 peer, bind and device cross
//! into this policy; tunnel packets, keys, routes and firewall are not involved.
use crate::config::{Role, Settings};
use std::{
    io,
    net::{SocketAddrV4, UdpSocket},
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    time::Duration,
};

pub struct Policy {
    entry: SocketAddrV4,
    bind: SocketAddrV4,
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
        socket.connect(SocketAddrV4::new(*self.entry.ip(), port))?;
        Ok(socket)
    }
    /// Serve at most one request, with bounded waiting so child exit/signals can
    /// always be observed by the supervisor. Malformed requests fail closed.
    pub fn serve_once(&self, channel: &UnixDatagram) -> io::Result<bool> {
        channel.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut request = [0u8; 3];
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
        if length != 2 {
            return Err(io::Error::other("invalid socket broker request"));
        }
        let port = u16::from_be_bytes([request[0], request[1]]);
        if port == 0 {
            return Err(io::Error::other("zero broker port"));
        }
        // Allocation failure is local to this candidate. A labeled no-FD reply
        // makes the worker reject it while retaining its existing working path.
        let socket = self.socket(port).ok();
        send_socket(channel, &request[..2], socket.as_ref())?;
        Ok(true)
    }
}
fn send_socket(channel: &UnixDatagram, label: &[u8], socket: Option<&UdpSocket>) -> io::Result<()> {
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
        policy.bind = match occupied.local_addr().unwrap() {
            std::net::SocketAddr::V4(v) => v,
            _ => unreachable!(),
        };
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
    fn rejects_malformed_and_zero_port_requests() {
        for request in [&[0, 0][..], &[1][..], &[1, 2, 3][..], &[1, 2, 3, 4][..]] {
            let (client, parent) = UnixDatagram::pair().unwrap();
            client.send(request).unwrap();
            assert!(policy().serve_once(&parent).is_err());
        }
    }
}
