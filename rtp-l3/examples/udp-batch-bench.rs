//! Synthetic loopback adapter experiment, not a router throughput benchmark.
#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use rtp_l3::v2::udp_io::{Backend, Datagram, ReceiveBatch, SendBatch};
    use std::{
        io,
        net::UdpSocket,
        time::{Duration, Instant},
    };
    let args: Vec<_> = std::env::args().collect();
    let backend = match args.get(1).map(String::as_str) {
        Some("portable") => Backend::Portable,
        #[cfg(target_os = "linux")]
        Some("mmsg") => Backend::Mmsg,
        _ => {
            return Err(io::Error::other(
                "usage: udp-batch-bench portable|mmsg BUDGET TURNS",
            ))
        }
    };
    let budget: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("invalid budget"))?;
    let turns: u64 = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0 && *n <= 1_000_000)
        .ok_or_else(|| io::Error::other("turns must be 1..1000000"))?;
    let mut tx = SendBatch::new(backend, budget)?;
    let mut rx = ReceiveBatch::new(backend, budget)?;
    let sender = UdpSocket::bind("127.0.0.1:0")?;
    let receiver = UdpSocket::bind("127.0.0.1:0")?;
    for s in [&sender, &receiver] {
        s.set_nonblocking(true)?;
        rtp_l3::worker::buffer_transport(s)?;
    }
    let source = sender.local_addr()?;
    let frame_len = rtp_l3::v2::codec::MAX_FRAME;
    let bodies: Vec<_> = (0..budget).map(|i| vec![i as u8; frame_len]).collect();
    let packets: Vec<_> = bodies
        .iter()
        .map(|frame| Datagram {
            frame,
            peer: receiver.local_addr().unwrap(),
        })
        .collect();
    fn cpu() -> f64 {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
        usage.ru_utime.tv_sec as f64
            + usage.ru_stime.tv_sec as f64
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1e6
    }
    let (start, before) = (Instant::now(), cpu());
    let (mut send_calls, mut recv_calls) = (0u64, 0u64);
    for _ in 0..turns {
        let mut sent = 0;
        while sent < packets.len() {
            sent += tx.send(&sender, &packets[sent..])?;
            send_calls += 1;
        }
        let mut received = 0;
        let deadline = Instant::now() + Duration::from_secs(1);
        while received < budget {
            recv_calls += 1;
            match rx.recv(&receiver) {
                Ok(got) => {
                    for p in got {
                        if p.peer != Some(source) || p.frame() != Some(bodies[received].as_slice())
                        {
                            return Err(io::Error::other(
                                "source, packet length or payload mismatch",
                            ));
                        }
                        received += 1;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::yield_now()
                }
                Err(e) => return Err(e),
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let seconds = cpu() - before;
    let datagrams = turns * budget as u64;
    println!(
        "{}",
        serde_json::json!({"backend": args[1], "budget": budget,
        "turns": turns, "datagrams": datagrams, "verified_bytes": datagrams * frame_len as u64,
        "elapsed_seconds": elapsed, "process_cpu_seconds": seconds,
        "cpu_ns_per_datagram": seconds * 1e9 / datagrams as f64,
        "send_adapter_calls": send_calls, "recv_adapter_calls": recv_calls,
        "scope": "synthetic single-thread loopback, no crypto/TUN/policy/WAN"})
    );
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    panic!("Unix UDP adapter experiment");
}
