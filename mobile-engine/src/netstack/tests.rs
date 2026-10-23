use super::*;
use etherparse::{PacketBuilder, TcpOptionElement};
use smoltcp::socket::tcp::Socket;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

struct PacketIo {
    rx: UnboundedReceiver<Vec<u8>>,
    tx: UnboundedSender<Vec<u8>>,
    syns: Arc<Mutex<Vec<Vec<u8>>>>,
    gate: Option<Arc<Gate>>,
}
#[derive(Default)]
struct Gate {
    blocked: std::sync::atomic::AtomicBool,
    waker: futures::task::AtomicWaker,
}
impl PacketIo {
    fn pair() -> (Self, Self) {
        let (a_tx, a_rx) = unbounded_channel();
        let (b_tx, b_rx) = unbounded_channel();
        (
            Self {
                rx: a_rx,
                tx: b_tx,
                syns: Arc::default(),
                gate: None,
            },
            Self {
                rx: b_rx,
                tx: a_tx,
                syns: Arc::default(),
                gate: None,
            },
        )
    }
}
impl AsyncRead for PacketIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(bytes)) => {
                out.put_slice(&bytes);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}
impl AsyncWrite for PacketIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Some(gate) = &self.gate {
            gate.waker.register(cx.waker());
            if gate.blocked.load(std::sync::atomic::Ordering::Relaxed) {
                return Poll::Pending;
            }
        }
        if let Ok(packet) = SlicedPacket::from_ip(bytes) {
            if matches!(packet.transport, Some(TransportSlice::Tcp(ref tcp)) if tcp.syn()) {
                self.syns.lock().unwrap().push(bytes.to_vec());
            }
        }
        self.tx.send(bytes.to_vec()).map_err(io::Error::other)?;
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn config(mtu: u16) -> Config {
    Config {
        mtu,
        tcp_buffer: 8192,
        tcp_idle_timeout: Duration::from_secs(3600),
        udp_idle_timeout: Duration::from_secs(60),
        min_rto: Duration::from_millis(200),
    }
}

struct Lab {
    stack: Stack,
    app: Stack,
    wire: UnboundedSender<Vec<u8>>,
    gate: Arc<Gate>,
    syns: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl Lab {
    fn new(mtu: u16) -> Self {
        let (mut server, client) = PacketIo::pair();
        let gate = Arc::new(Gate::default());
        server.gate = Some(gate.clone());
        let wire = client.tx.clone();
        let syns = server.syns.clone();
        Self {
            stack: Stack::new(config(mtu), server),
            app: Stack::new(config(mtu), client),
            wire,
            gate,
            syns,
        }
    }

    async fn connect(&mut self, dst: SocketAddr, port: u16) -> (TcpFlow, TcpFlow) {
        let src = SocketAddr::new(
            if dst.is_ipv4() {
                "192.0.2.2".parse().unwrap()
            } else {
                "2001:db8::2".parse().unwrap()
            },
            port,
        );
        let client = {
            let mut inner = self.app.shared.inner.lock().unwrap();
            let mut socket = Socket::new(
                smol_tcp::SocketBuffer::new(vec![0; 8192]),
                smol_tcp::SocketBuffer::new(vec![0; 8192]),
            );
            socket.set_nagle_enabled(false);
            socket.set_min_rto(smoltcp::time::Duration::from_millis(200));
            socket.connect(inner.iface.context(), dst, src).unwrap();
            let handle = inner.sockets.add(socket);
            let mut entry = tcp::Entry::new((dst, src));
            entry.accepted = true;
            let client = TcpFlow::new(self.app.shared.clone(), handle, dst, entry.control.clone());
            inner.tuples.insert((dst, src), handle);
            inner.tcp.insert(handle, entry);
            client
        };
        self.app.shared.notify.notify_one();
        let Flow::Tcp(server) = tokio::time::timeout(Duration::from_secs(5), self.stack.accept())
            .await
            .expect("handshake timed out")
            .unwrap()
        else {
            panic!("TCP flow");
        };
        (client, server)
    }
}

#[tokio::test]
async fn arbitrary_destinations_and_megabyte_transfers() {
    for dst in ["203.0.113.71:8080", "[2001:db8:1234::71]:8080"] {
        let mut lab = Lab::new(1500);
        let dst = dst.parse().unwrap();
        let (mut app, mut flow) = lab.connect(dst, 41000).await;
        assert_eq!(flow.peer_addr(), dst);
        let data: Vec<_> = (0..1_000_000).map(|i| (i % 251) as u8).collect();
        let mut received = vec![0; data.len()];
        let transfer = tokio::time::timeout(Duration::from_secs(10), async {
            let (sent, read) = tokio::join!(app.write_all(&data), flow.read_exact(&mut received));
            sent.unwrap();
            read.unwrap();
            assert_eq!(received, data);
            let (sent, read) = tokio::join!(flow.write_all(&data), app.read_exact(&mut received));
            sent.unwrap();
            read.unwrap();
            assert_eq!(received, data);
        })
        .await;
        transfer.unwrap();
    }
}

#[tokio::test]
async fn mss_matches_mtu_and_ip_version() {
    for mtu in [1280, 1500] {
        for dst in ["203.0.113.71:80", "[2001:db8::71]:80"] {
            let mut lab = Lab::new(mtu);
            let dst: SocketAddr = dst.parse().unwrap();
            let (_app, _flow) = lab.connect(dst, 41000).await;
            let syns = lab.syns.lock().unwrap();
            let packet = SlicedPacket::from_ip(&syns[0]).unwrap();
            let Some(TransportSlice::Tcp(tcp)) = packet.transport else {
                panic!();
            };
            assert!(tcp.syn() && tcp.ack());
            let mss = tcp.options_iterator().find_map(|o| match o.unwrap() {
                TcpOptionElement::MaximumSegmentSize(mss) => Some(mss),
                _ => None,
            });
            assert_eq!(mss, Some(mtu - if dst.is_ipv4() { 40 } else { 60 }));
        }
    }
}

#[tokio::test]
async fn fin_drains_data_and_shutdown_survives_drop() {
    let mut lab = Lab::new(1500);
    let (mut app, mut flow) = lab.connect("203.0.113.71:80".parse().unwrap(), 41000).await;
    app.write_all(b"request").await.unwrap();
    app.shutdown().await.unwrap();
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), flow.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, b"request");
    flow.write_all(b"response before FIN").await.unwrap();
    flow.shutdown().await.unwrap();
    drop(flow);
    received.clear();
    tokio::time::timeout(Duration::from_secs(5), app.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, b"response before FIN");
}

#[tokio::test]
async fn drop_resets_and_peer_reset_fails_reads_and_writes() {
    let mut lab = Lab::new(1500);
    let (mut app, flow) = lab.connect("203.0.113.71:80".parse().unwrap(), 41000).await;
    drop(flow);
    let error = tokio::time::timeout(Duration::from_secs(5), app.read(&mut [0; 1]))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    let (app, mut flow) = lab.connect("203.0.113.71:80".parse().unwrap(), 41001).await;
    drop(app);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), flow.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionReset
    );
    assert_eq!(
        flow.write(b"x").await.unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
}

#[tokio::test]
async fn same_destination_isolated_and_retransmitted_syn_has_no_duplicate_listener() {
    let mut lab = Lab::new(1500);
    let dst = "203.0.113.71:80".parse().unwrap();
    let (mut app1, mut flow1) = lab.connect(dst, 41000).await;
    let (mut app2, mut flow2) = lab.connect(dst, 41001).await;
    let syn = {
        // Retransmit the original first SYN after the connection is established (#1215).
        let builder = PacketBuilder::ipv4([192, 0, 2, 2], [203, 0, 113, 71], 64)
            .tcp(41000, 80, 1, 65535)
            .syn();
        let mut syn = Vec::new();
        builder.write(&mut syn, &[]).unwrap();
        syn
    };
    lab.wire.send(syn).unwrap();
    tokio::task::yield_now().await;
    app1.write_all(b"a").await.unwrap();
    app2.write_all(b"b").await.unwrap();
    let mut a = [0];
    let mut b = [0];
    tokio::time::timeout(Duration::from_secs(5), async {
        flow1.read_exact(&mut a).await.unwrap();
        flow2.read_exact(&mut b).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(a, *b"a");
    assert_eq!(b, *b"b");
    assert_eq!(lab.stack.shared.inner.lock().unwrap().tcp.len(), 2);
    assert!(lab.stack.flows.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn idle_timeout_and_stack_drop_wake_pending_flows() {
    let mut lab = Lab::new(1500);
    lab.stack
        .shared
        .inner
        .lock()
        .unwrap()
        .config
        .tcp_idle_timeout = Duration::from_secs(2);
    let (_app, mut flow) = lab.connect("203.0.113.71:80".parse().unwrap(), 41000).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), flow.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionReset
    );
    let (_app, mut flow) = lab.connect("203.0.113.71:80".parse().unwrap(), 41001).await;
    let shared = lab.stack.shared.clone();
    let reader = tokio::spawn(async move { flow.read(&mut [0; 1]).await });
    tokio::task::yield_now().await;
    drop(lab);
    assert!(!shared.inner.lock().unwrap().running);
    assert_eq!(
        reader.await.unwrap().unwrap_err().kind(),
        io::ErrorKind::ConnectionReset
    );
}

#[tokio::test(start_paused = true)]
async fn thousand_short_connections_are_reaped_after_time_wait() {
    let mut lab = Lab::new(1500);
    for i in 0..1000 {
        let (mut app, mut flow) = lab
            .connect("203.0.113.71:80".parse().unwrap(), 41000 + i)
            .await;
        flow.shutdown().await.unwrap();
        app.shutdown().await.unwrap();
        // Receive both FINs before advancing simulated time.
        assert_eq!(flow.read(&mut [0; 1]).await.unwrap(), 0);
        assert_eq!(app.read(&mut [0; 1]).await.unwrap(), 0);
        drop(flow);
        drop(app);
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
    // Give both drivers their timer wakeups.
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    let inner = lab.stack.shared.inner.lock().unwrap();
    assert!(inner.tcp.is_empty());
    assert!(inner.tuples.is_empty());
    assert_eq!(inner.sockets.iter().count(), 0);
}

fn udp_packet(src: SocketAddr, dst: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let builder = match (src, dst) {
        (SocketAddr::V4(src), SocketAddr::V4(dst)) => {
            PacketBuilder::ipv4(src.ip().octets(), dst.ip().octets(), 64)
        }
        (SocketAddr::V6(src), SocketAddr::V6(dst)) => {
            PacketBuilder::ipv6(src.ip().octets(), dst.ip().octets(), 64)
        }
        _ => unreachable!(),
    }
    .udp(src.port(), dst.port());
    let mut packet = Vec::new();
    builder.write(&mut packet, payload).unwrap();
    packet
}

#[tokio::test(start_paused = true)]
async fn udp_pairs_replies_datagrams_and_idle_eof() {
    for (src, dst) in [
        ("192.0.2.2:41000", "203.0.113.71:9999"),
        ("[2001:db8::2]:41000", "[2001:db8::71]:9999"),
    ] {
        let src: SocketAddr = src.parse().unwrap();
        let dst = dst.parse().unwrap();
        let (server, mut app) = PacketIo::pair();
        let mut stack = Stack::new(config(1500), server);
        app.write_all(&udp_packet(src, dst, b"one")).await.unwrap();
        let Flow::Udp(mut flow) = stack.accept().await.unwrap() else {
            panic!()
        };
        assert_eq!(flow.peer_addr(), dst);
        app.write_all(&udp_packet(src, dst, b"two")).await.unwrap();
        let mut bytes = [0; 1500];
        let n = flow.read(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..n], b"one");
        let n = flow.read(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..n], b"two");
        flow.write_all(b"reply").await.unwrap();
        let n = app.read(&mut bytes).await.unwrap();
        {
            let packet = SlicedPacket::from_ip(&bytes[..n]).unwrap();
            let Some(TransportSlice::Udp(udp)) = packet.transport else {
                panic!()
            };
            assert_eq!(udp.payload(), b"reply");
            assert_eq!(udp.source_port(), 9999);
            assert_eq!(udp.destination_port(), 41000);
        }
        let other = SocketAddr::new(src.ip(), 41001);
        app.write_all(&udp_packet(other, dst, b"other"))
            .await
            .unwrap();
        let Flow::Udp(mut second) = stack.accept().await.unwrap() else {
            panic!()
        };
        let n = second.read(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..n], b"other");
        tokio::time::sleep(Duration::from_secs(61)).await;
        assert_eq!(flow.read(&mut bytes).await.unwrap(), 0);
        assert_eq!(second.read(&mut bytes).await.unwrap(), 0);
        assert!(stack.shared.inner.lock().unwrap().udp.is_empty());
    }
}

#[tokio::test]
async fn device_write_backpressure_does_not_block_ingress() {
    let mut lab = Lab::new(1500);
    let (mut app, mut flow) = lab.connect("203.0.113.71:80".parse().unwrap(), 41000).await;
    lab.gate
        .blocked
        .store(true, std::sync::atomic::Ordering::Relaxed);
    flow.write_all(b"queued response").await.unwrap();
    tokio::task::yield_now().await;
    app.shutdown().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), flow.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    lab.gate
        .blocked
        .store(false, std::sync::atomic::Ordering::Relaxed);
    lab.gate.waker.wake();
    flow.shutdown().await.unwrap();
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), app.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, b"queued response");
}

/// Run in release mode on the profiling host; no live network or privileges are needed.
#[test]
#[ignore = "manual idle-socket CPU profile"]
fn profile_idle_socket_polling() {
    for count in [1, 100, 1000] {
        let mut device = device::Device::new(1500);
        let mut iface = Interface::new(
            InterfaceConfig::new(HardwareAddress::Ip),
            &mut device,
            SmolInstant::ZERO,
        );
        let mut sockets = SocketSet::new(vec![]);
        for i in 0..count {
            let mut socket = Socket::new(
                smol_tcp::SocketBuffer::new(vec![0; 1024]),
                smol_tcp::SocketBuffer::new(vec![0; 1024]),
            );
            socket.listen(10000 + i as u16).unwrap();
            sockets.add(socket);
        }
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            std::hint::black_box(iface.poll(SmolInstant::ZERO, &mut device, &mut sockets));
        }
        eprintln!(
            "{count} idle sockets: {:.3} microseconds per iface.poll",
            start.elapsed().as_secs_f64() * 10.0
        );
    }
}

#[tokio::test]
async fn simultaneous_megabyte_transfers_and_half_closes() {
    for dst in ["203.0.113.71:8080", "[2001:db8:1234::71]:8080"] {
        let mut lab = Lab::new(1500);
        let (app, flow) = lab.connect(dst.parse().unwrap(), 41000).await;
        let (mut app_read, mut app_write) = tokio::io::split(app);
        let (mut flow_read, mut flow_write) = tokio::io::split(flow);
        let to_stack = vec![0x31; 1_000_000];
        let to_app = vec![0x72; 1_000_000];
        let mut at_stack = Vec::new();
        let mut at_app = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::try_join!(
                async {
                    app_write.write_all(&to_stack).await?;
                    app_write.shutdown().await
                },
                async {
                    flow_write.write_all(&to_app).await?;
                    flow_write.shutdown().await
                },
                async { app_read.read_to_end(&mut at_app).await.map(|_| ()) },
                async { flow_read.read_to_end(&mut at_stack).await.map(|_| ()) },
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(at_stack, to_stack);
        assert_eq!(at_app, to_app);
    }
}
