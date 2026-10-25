use std::{
    net::UdpSocket,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use udp_relay::{Config, Mapping, Relay};
fn addr() -> std::net::SocketAddr {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn config(a: std::net::SocketAddr, b: std::net::SocketAddr) -> Config {
    Config {
        mappings: vec![Mapping {
            id: "one".into(),
            generation: "1".into(),
            listen: a,
            upstream: b,
            max_flows: 8,
        }],
        max_flows: 8,
        max_source_flows: 8,
        max_payload: 1200,
        socket_buffer: 8192,
        idle_seconds: 1,
        packets_per_second: 10000,
        bytes_per_second: 1000000,
        mapping_packets_per_second: 10000,
        mapping_bytes_per_second: 1000000,
        source_packets_per_second: 10000,
        source_bytes_per_second: 1000000,
    }
}
fn client() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    s
}
#[test]
fn payload_demux_translation_rebind_expiry_quota_restart() {
    let back = UdpSocket::bind("127.0.0.1:0").unwrap();
    back.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let upstream = back.local_addr().unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let s = stopped.clone();
    let echo = std::thread::spawn(move || {
        let mut b = [0; 65535];
        while !s.load(Ordering::Relaxed) {
            if let Ok((n, p)) = back.recv_from(&mut b) {
                back.send_to(&b[..n], p).unwrap();
            }
        }
    });
    let a = addr();
    let a2 = addr();
    let mut c = config(a, upstream);
    c.mappings.push(Mapping {
        id: "two".into(),
        generation: "2".into(),
        listen: a2,
        upstream,
        max_flows: 8,
    });
    c.max_flows = 3;
    c.max_source_flows = 3;
    for m in &mut c.mappings {
        m.max_flows = 3;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let mut relay = Relay::bind(c.clone()).unwrap();
    let worker = std::thread::spawn(move || {
        relay.run(&s);
        relay
    });
    let x = client();
    let y = client();
    let rebound = client();
    let mut out = [0; 2000];
    for (socket, dst, data) in [
        (&x, a, vec![]),
        (&y, a, (0..1200).map(|i| (i % 256) as u8).collect()),
        (&x, a2, b"second mapping".to_vec()),
    ] {
        socket.send_to(&data, dst).unwrap();
        let (n, p) = socket.recv_from(&mut out).unwrap();
        assert_eq!(p, dst);
        assert_eq!(&out[..n], data);
    }
    // Distinct flow sockets route delayed/interleaved responses to their original tuple.
    for i in 0..4u8 {
        x.send_to(&[i, 1], a).unwrap();
        y.send_to(&[i, 2], a).unwrap();
    }
    for i in 0..4u8 {
        let (n, p) = x.recv_from(&mut out).unwrap();
        assert_eq!((&out[..n], p), (&[i, 1][..], a));
        let (n, p) = y.recv_from(&mut out).unwrap();
        assert_eq!((&out[..n], p), (&[i, 2][..], a));
    }
    rebound.send_to(b"quota", a).unwrap();
    assert!(rebound.recv_from(&mut out).is_err());
    x.send_to(&vec![0; 1201], a).unwrap();
    assert!(x.recv_from(&mut out).is_err());
    std::thread::sleep(Duration::from_millis(1100));
    rebound.send_to(b"new tuple", a).unwrap();
    let (n, p) = rebound.recv_from(&mut out).unwrap();
    assert_eq!((&out[..n], p), (&b"new tuple"[..], a));
    stop.store(true, Ordering::Relaxed);
    let relay = worker.join().unwrap();
    assert!(relay.counters.expired >= 3);
    assert!(relay.counters.quota_drops >= 1);
    assert!(relay.counters.oversized >= 1);
    assert!(relay.counters.created >= 4);
    drop(relay);
    let mut relay = Relay::bind(c).unwrap();
    let stop2 = Arc::new(AtomicBool::new(false));
    let s = stop2.clone();
    let w = std::thread::spawn(move || relay.run(&s));
    x.send_to(b"restart", a).unwrap();
    let (n, p) = x.recv_from(&mut out).unwrap();
    assert_eq!((&out[..n], p), (&b"restart"[..], a));
    stop2.store(true, Ordering::Relaxed);
    w.join().unwrap();
    stopped.store(true, Ordering::Relaxed);
    echo.join().unwrap();
}
#[test]
fn independent_limits_and_atomic_bind() {
    let a = addr();
    let b = addr();
    let mut c = config(a, b);
    c.max_source_flows = 1;
    let mut relay = Relay::bind(c.clone()).unwrap();
    assert!(Relay::bind(c.clone()).is_err());
    let x = client();
    let y = client();
    x.send_to(b"a", a).unwrap();
    y.send_to(b"b", a).unwrap();
    relay.tick();
    assert_eq!(relay.counters.active, 1);
    assert_eq!(relay.counters.quota_drops, 1);
    drop(relay);
    c.source_packets_per_second = 1;
    let mut relay = Relay::bind(c).unwrap();
    x.send_to(b"a", a).unwrap();
    x.send_to(b"b", a).unwrap();
    relay.tick();
    assert_eq!(relay.counters.rate_drops, 1);
}

#[test]
fn delayed_reply_stays_with_old_tuple_and_oversize_reply_drops() {
    let backend = UdpSocket::bind("127.0.0.1:0").unwrap();
    backend
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let upstream = backend.local_addr().unwrap();
    let a = addr();
    let mut relay = Relay::bind(config(a, upstream)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let worker = std::thread::spawn(move || {
        relay.run(&s);
        relay
    });
    let old = client();
    let new = client();
    let mut b = [0; 2000];
    old.send_to(b"old", a).unwrap();
    let (n, old_flow) = backend.recv_from(&mut b).unwrap();
    assert_eq!(&b[..n], b"old");
    new.send_to(b"new", a).unwrap();
    let (n, new_flow) = backend.recv_from(&mut b).unwrap();
    assert_eq!(&b[..n], b"new");
    assert_ne!(new_flow, old_flow);
    backend.send_to(b"delayed-old", old_flow).unwrap();
    backend.send_to(b"new-only", new_flow).unwrap();
    let (n, p) = new.recv_from(&mut b).unwrap();
    assert_eq!((&b[..n], p), (&b"new-only"[..], a));
    let (n, p) = old.recv_from(&mut b).unwrap();
    assert_eq!((&b[..n], p), (&b"delayed-old"[..], a));
    backend.send_to(&vec![1; 4000], old_flow).unwrap();
    assert!(old.recv_from(&mut b).is_err());
    // Connected flow socket rejects datagrams from another upstream tuple.
    let spoof = client();
    spoof.send_to(b"foreign", old_flow).unwrap();
    assert!(old.recv_from(&mut b).is_err());
    stop.store(true, Ordering::Relaxed);
    assert_eq!(worker.join().unwrap().counters.oversized, 1);
}
