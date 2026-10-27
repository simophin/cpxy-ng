//! Real loopback HTTP+UDP; packet-preserving descriptors stand in for TUN.
use super::{client::Client, server, transport};
use crate::{
    cli::{Cli, RoleCommand},
    config::Settings,
};
use clap::Parser;
use rtp_l3::worker::{PacketIo, SocketProvider};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixDatagram,
    },
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;
struct Device(UnixDatagram);
impl AsRawFd for Device {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}
impl Read for Device {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.recv(b)
    }
}
impl Write for Device {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.send(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Default)]
struct Host {
    ports: Mutex<BTreeSet<u16>>,
}
impl SocketProvider for Host {
    fn prepare_port(&self, a: SocketAddr) -> io::Result<UdpSocket> {
        let s = UdpSocket::bind(a)?;
        assert!(self.ports.lock().unwrap().insert(s.local_addr()?.port()));
        Ok(s)
    }
    fn commit_port(&self, p: u16) -> io::Result<()> {
        assert!(self.ports.lock().unwrap().contains(&p));
        Ok(())
    }
    fn retire_port(&self, p: u16) -> io::Result<()> {
        self.ports.lock().unwrap().remove(&p);
        Ok(())
    }
    fn reconcile_ports(&self) -> io::Result<()> {
        Ok(())
    }
    fn client_socket(&self, peer: SocketAddr) -> io::Result<UdpSocket> {
        UdpSocket::bind(if peer.is_ipv4() {
            "127.0.0.1:0"
        } else {
            "[::1]:0"
        })
    }
    fn control_socket(&self, peer: SocketAddr) -> io::Result<TcpStream> {
        TcpStream::connect_timeout(&peer, Duration::from_secs(2))
    }
}
struct Task {
    run: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.run.store(false, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            assert!(t.join().unwrap().is_ok());
        }
    }
}
struct Lab {
    root: std::path::PathBuf,
    server: Option<Task>,
    entry: SocketAddr,
    host: Arc<Host>,
    packet: UnixDatagram,
}
impl Lab {
    fn new() -> Self {
        Self::with_port_selection(false)
    }
    fn with_port_selection(automatic: bool) -> Self {
        let root = std::env::temp_dir().join(format!("rtp-http-lab-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let entry = listener.local_addr().unwrap();
        drop(listener);
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let low = udp.local_addr().unwrap().port().min(65000);
        drop(udp);
        let mut settings = endpoint(entry, low, &root);
        if automatic {
            settings.transport = crate::config::Role::Endpoint {
                listen: entry,
                udp_low: 0,
                udp_high: 0,
            };
            settings.validate().unwrap();
        }
        let host = Arc::new(Host::default());
        let run = Arc::new(AtomicBool::new(true));
        let (packet, device) = UnixDatagram::pair().unwrap();
        packet
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let h = host.clone();
        let r = run.clone();
        let t = thread::spawn(move || {
            server::run(
                settings,
                [7; 32],
                Box::new(Device(device)),
                h,
                Arc::new(move |e| {
                    let _ = tx.send(e);
                }),
                r,
            )
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap()["event"],
            "ready"
        );
        Self {
            root,
            server: Some(Task {
                run,
                thread: Some(t),
            }),
            entry,
            host,
            packet,
        }
    }
    fn control(&self, id: Uuid) -> Client {
        Client::new(&[7; 32], "cpxy-rtp", self.entry, self.host.clone(), id).unwrap()
    }
    fn client(&self, ip: &str) -> (Task, UnixDatagram, mpsc::Receiver<Value>) {
        let args = [
            "rtp",
            "client",
            "--entry",
            &self.entry.to_string(),
            "--outbound-interface",
            "lo",
            "--address",
            ip,
        ];
        let RoleCommand::Client { args, .. } = Cli::parse_from(args).role else {
            panic!()
        };
        let settings = args.settings().unwrap();
        let control = self.control(Uuid::new_v4());
        let (packet, device) = UnixDatagram::pair().unwrap();
        packet
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let run = Arc::new(AtomicBool::new(true));
        let r = run.clone();
        let host = self.host.clone();
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            transport::run(
                settings,
                control,
                Box::new(Device(device)) as Box<dyn PacketIo>,
                host,
                Arc::new(move |e| {
                    let _ = tx.send(e);
                }),
                r,
            )
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap()["event"],
            "connected"
        );
        (
            Task {
                run,
                thread: Some(thread),
            },
            packet,
            rx,
        )
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        drop(self.server.take());
        assert!(self.host.ports.lock().unwrap().is_empty());
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn endpoint(entry: SocketAddr, low: u16, root: &std::path::Path) -> Settings {
    let RoleCommand::Endpoint { args, .. } = Cli::parse_from([
        "rtp",
        "endpoint",
        "--listen",
        &entry.to_string(),
        "--udp-ports",
        &format!("{low}-{}", low + 8),
        "--firewall",
        "none",
        "--state-dir",
        root.to_str().unwrap(),
    ])
    .role
    else {
        panic!()
    };
    args.settings().unwrap()
}
fn packet(source: [u8; 4], destination: [u8; 4], serial: u8) -> Vec<u8> {
    let mut ip = vec![serial; 60];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&60u16.to_be_bytes());
    ip[12..16].copy_from_slice(&source);
    ip[16..20].copy_from_slice(&destination);
    ip
}
fn transfer(source: &UnixDatagram, destination: &UnixDatagram, ip: &[u8]) {
    source.send(ip).unwrap();
    let mut b = [0; 1201];
    let n = destination.recv(&mut b).unwrap();
    assert_eq!(&b[..n], ip);
}
#[test]
fn two_clients_real_http_udp_and_exact_packet_dispatch() {
    let lab = Lab::new();
    let (a, ap, _) = lab.client("10.88.0.2/24");
    let (b, bp, _) = lab.client("10.88.0.3/24");
    for i in 0..20 {
        transfer(&ap, &lab.packet, &packet([10, 88, 0, 2], [10, 90, 0, 2], i));
        transfer(&bp, &lab.packet, &packet([10, 88, 0, 3], [10, 90, 0, 2], i));
        transfer(&lab.packet, &ap, &packet([10, 90, 0, 2], [10, 88, 0, 2], i));
        transfer(&lab.packet, &bp, &packet([10, 90, 0, 2], [10, 88, 0, 3], i));
    }
    drop(a);
    drop(b);
}
#[test]
fn automatic_udp_ports_are_committed_and_support_two_clients() {
    let lab = Lab::with_port_selection(true);
    let (a, ap, _) = lab.client("10.88.0.2/24");
    let (b, bp, _) = lab.client("10.88.0.3/24");
    let ports = lab.host.ports.lock().unwrap().clone();
    assert_eq!(ports.len(), 2);
    assert!(!ports.contains(&0));
    transfer(&ap, &lab.packet, &packet([10, 88, 0, 2], [10, 90, 0, 2], 1));
    transfer(&lab.packet, &ap, &packet([10, 90, 0, 2], [10, 88, 0, 2], 2));
    transfer(&bp, &lab.packet, &packet([10, 88, 0, 3], [10, 90, 0, 2], 3));
    transfer(&lab.packet, &bp, &packet([10, 90, 0, 2], [10, 88, 0, 3], 4));
    drop(a);
    drop(b);
    assert!(lab.host.ports.lock().unwrap().is_empty());
}
#[test]
fn address_and_create_retry_release_terminal_and_static_conflict() {
    let lab = Lab::new();
    let mut c = lab.control(Uuid::new_v4());
    let address = c.call("allocate_ip", Default::default()).unwrap();
    assert_eq!(address["inner"], "10.88.0.2");
    let op = c
        .operation(
            "create_udp",
            json!({"inner":address["inner"]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let a = c.execute(&op).unwrap();
    let b = c.execute(&op).unwrap();
    assert_eq!(a, b);
    assert_eq!(lab.host.ports.lock().unwrap().len(), 1);
    let mut other = lab.control(Uuid::new_v4());
    let error = other
        .call(
            "create_udp",
            json!({"inner":address["inner"]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "authenticated control error -32005");
    let release = c
        .operation(
            "release_udp",
            json!({"allocation_id":a["allocation_id"]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    assert_eq!(c.execute(&release).unwrap(), json!({"released":true}));
    assert_eq!(c.execute(&release).unwrap(), json!({"released":true}));
    assert_eq!(
        c.execute(&op).unwrap_err().to_string(),
        "authenticated control error -32013"
    );
    assert!(lab.host.ports.lock().unwrap().is_empty());
}
#[test]
#[ignore = "exercises the production five-minute activation schedule"]
fn five_minute_rotation_keeps_bidirectional_traffic_and_retires_old_lease() {
    let lab = Lab::new();
    let (task, client, events) = lab.client("10.88.0.2/24");
    let started = Instant::now();
    loop {
        transfer(
            &client,
            &lab.packet,
            &packet([10, 88, 0, 2], [10, 90, 0, 2], 1),
        );
        transfer(
            &lab.packet,
            &client,
            &packet([10, 90, 0, 2], [10, 88, 0, 2], 2),
        );
        if events.try_iter().any(|e| e["event"] == "connected") {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(315));
        thread::sleep(Duration::from_millis(20));
    }
    assert!(started.elapsed() >= Duration::from_secs(299));
    thread::sleep(Duration::from_secs(11));
    assert_eq!(lab.host.ports.lock().unwrap().len(), 1);
    drop(task);
}

#[test]
fn lost_activation_ack_roaming_and_delayed_old_path_cannot_restore_route() {
    use rtp_l3::v2::{
        codec::{Control, Direction, Packet, Rx, Tx},
        http::unhex,
    };
    let lab = Lab::new();
    let mut control = lab.control(Uuid::new_v4());
    let allocation = control
        .call(
            "create_udp",
            json!({"inner":"10.88.0.2"}).as_object().unwrap().clone(),
        )
        .unwrap();
    let id: Uuid = serde_json::from_value(allocation["allocation_id"].clone()).unwrap();
    let secret = unhex::<32>(allocation["secret"].as_str().unwrap()).unwrap();
    let inner = "10.88.0.2".parse().unwrap();
    let mut tx = Tx::new(&secret, *id.as_bytes(), Direction::Upstream, inner);
    let mut rx = Rx::new(&secret, *id.as_bytes(), Direction::Downstream, inner);
    let mut peer = lab.entry;
    peer.set_port(allocation["port"].as_u64().unwrap() as u16);
    let old = UdpSocket::bind("127.0.0.1:0").unwrap();
    let new = UdpSocket::bind("127.0.0.1:0").unwrap();
    for s in [&old, &new] {
        s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    }
    let send = |tx: &mut Tx, s: &UdpSocket, c: Control| {
        s.send_to(&tx.control(c, 0).unwrap(), peer).unwrap();
    };
    let receive = |rx: &mut Rx, s: &UdpSocket| {
        let mut b = [0; 1300];
        let n = s.recv(&mut b).unwrap();
        rx.open(&b[..n]).unwrap()
    };
    let validate = |tx: &mut Tx, rx: &mut Rx, s: &UdpSocket, attempt, token| {
        send(tx, s, Control::PathOpen { attempt, token });
        let Packet::Control(Control::PathChallenge { challenge, .. }) = receive(rx, s) else {
            panic!()
        };
        send(
            tx,
            s,
            Control::PathResponse {
                attempt,
                token,
                challenge,
            },
        );
        assert_eq!(
            receive(rx, s),
            Packet::Control(Control::PathReady { attempt, token })
        );
    };
    // Queue exists once the allocation is committed, before UDP is ready.
    let downstream = packet([10, 90, 0, 2], [10, 88, 0, 2], 1);
    lab.packet.send(&downstream).unwrap();
    thread::sleep(Duration::from_millis(20));
    validate(&mut tx, &mut rx, &old, 1, [1; 16]);
    let activate = Control::Activate {
        activation: 1,
        attempt: 1,
        token: [1; 16],
    };
    send(&mut tx, &old, activate);
    // Discard ACTIVATED to simulate a lost reply. The server already owns the
    // route and flushes its pending FIFO before any newer packet.
    let mut lost = [0; 1300];
    old.recv(&mut lost).unwrap();
    assert_eq!(receive(&mut rx, &old), Packet::Data(downstream.clone()));
    send(&mut tx, &old, activate);
    assert_eq!(
        receive(&mut rx, &old),
        Packet::Control(Control::Activated {
            activation: 1,
            attempt: 1,
            token: [1; 16]
        })
    );
    validate(&mut tx, &mut rx, &new, 2, [2; 16]);
    // Validation alone does not redirect downstream.
    lab.packet.send(&downstream).unwrap();
    assert_eq!(receive(&mut rx, &old), Packet::Data(downstream.clone()));
    send(
        &mut tx,
        &new,
        Control::Activate {
            activation: 2,
            attempt: 2,
            token: [2; 16],
        },
    );
    assert_eq!(
        receive(&mut rx, &new),
        Packet::Control(Control::Activated {
            activation: 2,
            attempt: 2,
            token: [2; 16]
        })
    );
    // A fresh authenticated delayed old attempt still cannot roll the tuple back.
    send(
        &mut tx,
        &old,
        Control::PathOpen {
            attempt: 1,
            token: [1; 16],
        },
    );
    send(&mut tx, &old, activate);
    lab.packet.send(&downstream).unwrap();
    assert_eq!(receive(&mut rx, &new), Packet::Data(downstream));
    let upstream = packet([10, 88, 0, 2], [10, 90, 0, 2], 2);
    new.send_to(&tx.data(&upstream, 0).unwrap(), peer).unwrap();
    let mut b = [0; 1200];
    let n = lab.packet.recv(&mut b).unwrap();
    assert_eq!(&b[..n], upstream);
}

#[test]
fn concurrent_duplicate_creates_share_one_prepared_lease() {
    let lab = Lab::new();
    let id = Uuid::new_v4();
    let mut a = lab.control(id);
    let mut b = lab.control(id);
    let operation = a
        .operation(
            "create_udp",
            json!({"inner":"10.88.0.2"}).as_object().unwrap().clone(),
        )
        .unwrap();
    let duplicate = operation.clone();
    let first = thread::spawn(move || a.execute(&operation).unwrap());
    let second = thread::spawn(move || b.execute(&duplicate).unwrap());
    assert_eq!(first.join().unwrap(), second.join().unwrap());
    assert_eq!(lab.host.ports.lock().unwrap().len(), 1);
}
