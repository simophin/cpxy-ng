use super::{Flow, Inner, Pair, Shared, device::QUEUE_LEN, reset};
use etherparse::PacketBuilder;
use futures::task::AtomicWaker;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::Instant;

pub(super) struct Entry {
    pub tx: mpsc::Sender<Vec<u8>>,
    pub activity: Instant,
    // Prevent a flow from writing through a newly created entry with the same tuple.
    pub(super) identity: Arc<AtomicWaker>,
}

pub(crate) struct UdpFlow {
    shared: Arc<Shared>,
    pair: Pair,
    rx: mpsc::Receiver<Vec<u8>>,
    identity: Arc<AtomicWaker>,
}

impl Inner {
    pub(super) fn ingest_udp(
        &mut self,
        pair: Pair,
        payload: &[u8],
        shared: &Arc<Shared>,
        flows: &mpsc::Sender<Flow>,
    ) {
        if !self.udp.contains_key(&pair) {
            let Ok(permit) = flows.try_reserve() else {
                return;
            };
            let (tx, rx) = mpsc::channel(64);
            let identity = Arc::new(AtomicWaker::new());
            self.udp.insert(
                pair,
                Entry {
                    tx,
                    activity: Instant::now(),
                    identity: identity.clone(),
                },
            );
            permit.send(Flow::Udp(UdpFlow {
                shared: shared.clone(),
                pair,
                rx,
                identity,
            }));
        }
        let entry = self.udp.get_mut(&pair).unwrap();
        entry.activity = Instant::now();
        // UDP loss when the consumer falls behind is preferable to unbounded memory use.
        let _ = entry.tx.try_send(payload.to_vec());
    }
}

impl UdpFlow {
    pub fn peer_addr(&self) -> SocketAddr {
        self.pair.1
    }
}

impl AsyncRead for UdpFlow {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.shared.inner.lock().unwrap().running {
            return Poll::Ready(Err(reset()));
        }
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(packet)) => {
                out.put_slice(&packet[..packet.len().min(out.remaining())]);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for UdpFlow {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut inner = self.shared.inner.lock().unwrap();
        if !inner.running {
            return Poll::Ready(Err(reset()));
        }
        let Some(entry) = inner.udp.get(&self.pair) else {
            return Poll::Ready(Err(reset()));
        };
        if !Arc::ptr_eq(&entry.identity, &self.identity) {
            return Poll::Ready(Err(reset()));
        }
        if inner.device.tx.len() >= QUEUE_LEN {
            self.identity.register(cx.waker());
            return Poll::Pending;
        }
        let (src, dst) = (self.pair.1, self.pair.0);
        let builder = match (src, dst) {
            (SocketAddr::V4(src), SocketAddr::V4(dst)) => {
                PacketBuilder::ipv4(src.ip().octets(), dst.ip().octets(), 64)
            }
            (SocketAddr::V6(src), SocketAddr::V6(dst)) => {
                PacketBuilder::ipv6(src.ip().octets(), dst.ip().octets(), 64)
            }
            _ => unreachable!("matching IP versions"),
        }
        .udp(src.port(), dst.port());
        let mut packet = Vec::with_capacity(builder.size(bytes.len()));
        builder
            .write(&mut packet, bytes)
            .map_err(io::Error::other)?;
        if packet.len() > inner.config.mtu as usize {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP datagram exceeds MTU",
            )));
        }
        inner.udp.get_mut(&self.pair).unwrap().activity = Instant::now();
        inner.device.tx.push_back(packet);
        self.shared.notify.notify_one();
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for UdpFlow {
    fn drop(&mut self) {
        self.rx.close();
        self.shared.notify.notify_one();
    }
}
