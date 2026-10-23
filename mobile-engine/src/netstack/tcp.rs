use super::{Pair, Shared, reset};
use futures::task::AtomicWaker;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::{RecvError, Socket};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Instant;

#[derive(Default)]
pub(super) struct Control {
    failed: AtomicBool,
    recv: AtomicWaker,
    send: AtomicWaker,
}
impl Control {
    pub fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.recv.wake();
        self.send.wake();
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

pub(super) struct Entry {
    pub pair: Pair,
    pub activity: Instant,
    pub accepted: bool,
    pub dropped: bool,
    pub shutdown: bool,
    pub control: Arc<Control>,
}
impl Entry {
    pub fn new(pair: Pair) -> Self {
        Self {
            pair,
            activity: Instant::now(),
            accepted: false,
            dropped: false,
            shutdown: false,
            control: Arc::default(),
        }
    }
}

pub(crate) struct TcpFlow {
    shared: Arc<Shared>,
    handle: SocketHandle,
    peer: SocketAddr,
    control: Arc<Control>,
}
impl TcpFlow {
    pub(super) fn new(
        shared: Arc<Shared>,
        handle: SocketHandle,
        peer: SocketAddr,
        control: Arc<Control>,
    ) -> Self {
        Self {
            shared,
            handle,
            peer,
            control,
        }
    }
    /// The destination the app connected to, matching ipstack's naming.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }
}

impl AsyncRead for TcpFlow {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut inner = self.shared.inner.lock().unwrap();
        if !inner.running || self.control.failed() {
            return Poll::Ready(Err(reset()));
        }
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let socket = inner.sockets.get_mut::<Socket>(self.handle);
        match socket.recv_slice(out.initialize_unfilled()) {
            Ok(0) => {
                self.control.recv.register(cx.waker());
                socket.register_recv_waker(cx.waker());
                Poll::Pending
            }
            Ok(n) => {
                out.advance(n);
                inner.tcp.get_mut(&self.handle).unwrap().activity = Instant::now();
                self.shared.notify.notify_one();
                Poll::Ready(Ok(()))
            }
            Err(RecvError::Finished) => Poll::Ready(Ok(())),
            Err(RecvError::InvalidState) => Poll::Ready(Err(reset())),
        }
    }
}

impl AsyncWrite for TcpFlow {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut inner = self.shared.inner.lock().unwrap();
        if !inner.running || self.control.failed() {
            return Poll::Ready(Err(reset()));
        }
        if inner.tcp.get(&self.handle).is_none_or(|e| e.shutdown) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TCP write half shut down",
            )));
        }
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let socket = inner.sockets.get_mut::<Socket>(self.handle);
        match socket.send_slice(bytes) {
            Ok(0) => {
                self.control.send.register(cx.waker());
                socket.register_send_waker(cx.waker());
                Poll::Pending
            }
            Ok(n) => {
                inner.tcp.get_mut(&self.handle).unwrap().activity = Instant::now();
                self.shared.notify.notify_one();
                Poll::Ready(Ok(n))
            }
            Err(_) => Poll::Ready(Err(reset())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.shared.inner.lock().unwrap();
        if !inner.running || self.control.failed() {
            return Poll::Ready(Err(reset()));
        }
        let socket = inner.sockets.get_mut::<Socket>(self.handle);
        if socket.send_queue() == 0 {
            return Poll::Ready(Ok(()));
        }
        self.control.send.register(cx.waker());
        socket.register_send_waker(cx.waker());
        self.shared.notify.notify_one();
        Poll::Pending
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut inner = self.shared.inner.lock().unwrap();
        if !inner.running || self.control.failed() {
            return Poll::Ready(Err(reset()));
        }
        inner.tcp.get_mut(&self.handle).unwrap().shutdown = true;
        inner.sockets.get_mut::<Socket>(self.handle).close();
        self.shared.notify.notify_one();
        Poll::Ready(Ok(()))
    }
}

impl Drop for TcpFlow {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock().unwrap();
        if !inner.running || self.control.failed() {
            return;
        }
        if let Some(entry) = inner.tcp.get_mut(&self.handle) {
            // A failed/reaped socket cannot be confused with a reused SocketHandle.
            if !Arc::ptr_eq(&entry.control, &self.control) {
                return;
            }
            entry.dropped = true;
            if !entry.shutdown {
                inner.sockets.get_mut::<Socket>(self.handle).abort();
            }
            self.shared.notify.notify_one();
        }
    }
}
