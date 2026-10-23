# Replacing ipstack with smoltcp: plan

Status: Phase 0 measurements running on `cn-baise`; Phase 1 implemented (nine
packet-level tests pass locally), Linux CI pending. Phase 2 integration implemented; Linux validation and measurements pending.
Phases 3–4 pending. Phase 5 remains outside this PR's requested scope.

## Why

The packet engine (`mobile-engine`) turns the IP packets of a TUN device, or of cpxy-tsgw's
socketpair, into TCP and UDP flows with the [`ipstack`](https://github.com/simophin/ipstack)
fork. ipstack has **no congestion control**: it sends up to the configured window
(`TCP_WINDOW`, 256 KiB for the gateway) at once. A window larger than the path's
bandwidth x round trip floods the bottleneck. Across China at 54 ms, 1 MiB moved 1 MB/s where
256 KiB moved 4-5 MB/s, so the window is hand-tuned per path today (see `GATEWAY_TCP_WINDOW` in
`mobile-engine/src/lib.rs` and `TCP_WINDOW` in [cpxy-tsgw.md](cpxy-tsgw.md)).

ipstack also has no round-trip estimator (fixed 1 s retransmit timeout), and its fast
retransmit resends the same segment on every duplicate ACK past the third.

[smoltcp](https://github.com/smoltcp-rs/smoltcp) 0.14 has Reno and CUBIC (RFC 9438), an
RFC 6298 RTT estimator, window scaling and timestamps, and ten years of state-machine fixes. It
is also already in our dependency tree through tailscale-rs. Its loss *recovery* is incomplete,
and this plan fills that in:

| Gap in smoltcp 0.14 | Effect on a lossy 54 ms path | Plan |
|---|---|---|
| Minimum RTO is 1 s (`RTTE_MIN_RTO`) | Every timeout stalls the flow for a second | Use `set_min_rto` (upstream #1211, on main, unreleased): 200 ms |
| No NewReno partial-ACK handling: the first new ACK ends fast recovery | A second loss in the same window waits for a timeout | Patch our fork (phase 3) |
| A timeout rewinds and resends the whole window (go-back-N) | Wasted bandwidth after a timeout | Accept for now; SACK (phase 4) fixes it properly |
| Received SACK blocks are ignored; only one SACK block is sent | Slow recovery from multiple losses | Upstream work in progress (#1195 receiver side; sender side to follow); phase 4 |
| Open bug #1221: pure ACKs during RTO recovery carry the rewound SEQ, which the peer rejects | Possible stall of a flow sending both ways while losing packets | Patch our fork (phase 3) |
| Open bug #1215: a listening socket can take a retransmitted SYN of an established connection | Duplicate connection | Our glue only creates a listener for a 4-tuple it does not know (phase 1) |

Someone on smoltcp #256 measured a Rust Tailscale exit node over noisy Wi-Fi: stock smoltcp
spent 14-19% of download time stalled in 1 s timeouts; with bounded sender SACK, NewReno and a
200 ms minimum RTO, stalls fell under 1% and throughput rose from ~9.3 to ~15-16 Mbit/s.

### Not in scope

- **The tsgw upload stall** (a sustained upload stops for good after minutes; see PR #134). It
  points at magicsock's receive path in tailscale-rs, not at the TCP stack. Do not expect this
  work to fix it, and keep benchmark uploads short (100 MB) so it does not spoil measurements.
- ICMP. The engine relays none today (ipstack's `UnknownTransport` is dropped); keep it so.
- Changing routing, DNS split, SNI reporting or the packet filter (`filter.rs`). They sit in
  front of or behind the stack and must not change.

## Decisions

- **Fork smoltcp** as `simophin/smoltcp`, branch `cpxy`, based on upstream `main` (it needs
  #1211). Depend on it by git rev in `mobile-engine/Cargo.toml`, like ipstack. Do **not** add a
  `[patch.crates-io]`: tailscale-rs keeps its crates.io smoltcp 0.14, and the two copies are
  separate crates.
- **Our own glue, in-tree** (`mobile-engine/src/netstack/`), not the `netstack-smoltcp` crate.
  That crate is on smoltcp 0.12 without congestion control, and we need per-socket settings and
  close/reset behaviour that match what the engine relies on. Read it and
  [embassy-net](https://github.com/embassy-rs/embassy/tree/main/embassy-net)'s `tcp.rs` for
  how to wrap smoltcp sockets as async streams; do not copy either wholesale.
- **TCP only through smoltcp.** UDP is parsed and built with `etherparse` (already a dependency;
  `filter.rs` builds ICMP replies with it). Any other packet is dropped.
- **CUBIC** on every socket, Nagle off.
- **Switched by a cargo feature during the transition**, `smoltcp` in `mobile-engine`, off by
  default. tsgw turns it on first (phase 2); mobile and the router follow (phase 5), then
  ipstack goes.

## The glue: `mobile-engine/src/netstack/`

It must offer what `run()` in `mobile-engine/src/lib.rs` uses from ipstack today, so the swap in
`lib.rs` is small:

```rust
pub struct Config { pub mtu: u16, pub tcp_buffer: usize, pub tcp_idle_timeout: Duration,
                    pub udp_idle_timeout: Duration, pub min_rto: Duration }
pub struct Stack;              // Stack::new(config, device: TunDevice) -> Stack
                               // async fn accept(&mut self) -> io::Result<Flow>
pub enum Flow { Tcp(TcpFlow), Udp(UdpFlow) }
pub struct TcpFlow;            // AsyncRead + AsyncWrite + Unpin + Send, fn peer_addr() -> SocketAddr
pub struct UdpFlow;            // AsyncRead + AsyncWrite (one datagram per read/write), fn peer_addr()
```

`peer_addr()` is the **destination** the app connected to (that is what ipstack's
`peer_addr()` returns and what `handle_tcp` routes on). Keep the same naming.

Suggested files: `mod.rs` (Stack, accept, the poll task), `device.rs` (smoltcp `Device` over
packet queues), `tcp.rs` (TcpFlow), `udp.rs` (UDP flow table and UdpFlow).

### Shape

- One **stack task** owns an `Arc<Mutex<Inner>>` holding the smoltcp `Interface`,
  `SocketSet<'static>` (created with `SocketSet::new(vec![])` so it grows), the 4-tuple →
  `SocketHandle` map, and the queues of the smoltcp `Device`.
- The task loops on `select!` of: a packet read from the `TunDevice`, a `Notify` that a flow
  touched its socket, the `iface.poll_delay()` timer, and shutdown. After each wake it locks,
  calls `iface.poll(...)`, writes out queued packets, and reaps sockets.
- `TcpFlow` holds the `Arc`, its `SocketHandle` and the `Notify`. `poll_read` locks, calls
  `recv_slice`; if nothing, `register_recv_waker(cx.waker())` and returns `Pending`.
  `poll_write` likewise with `send_slice` / `register_send_waker`. **After any successful
  read or write, notify the stack task**: smoltcp only sends (data, or a window update after a
  read) from `iface.poll`. This needs smoltcp's `async` feature.
- Never hold the mutex across an `.await`. A `std::sync::Mutex` is fine.
- Time: smoltcp's `Instant` from `tokio::time::Instant` elapsed since the stack started, so
  tests with `tokio::time::pause()` work.

### Interface

- `Medium::Ip`, `iface.set_any_ip(true)`, default IPv4 and IPv6 routes via an address on the
  interface (any-IP only accepts packets that route to the interface). Give the interface a
  placeholder address per family, e.g. `10.255.255.1/32` and `fd00:a:e9:ffff::1/128`; they are
  never used as a source because every socket is bound to the app's destination.
- `DeviceCapabilities`: `medium = Ip`, `max_transmission_unit = mtu`, `max_burst_size = None`.
  smoltcp derives the MSS it advertises from the MTU per IP version, which replaces
  `mss_for_mtu`; check it with a test (1280 → 1240 on IPv4, 1220 on IPv6).
- Checksums: keep smoltcp's default (verify and compute) first. Turning off receive
  verification is a later optimisation, only with a benchmark showing it matters.
- Only the smoltcp features needed: `std`, `medium-ip`, `proto-ipv4`, `proto-ipv6`,
  `socket-tcp`, `socket-tcp-cubic`, `async`, `log` off (or route it to `tracing`).

### Accepting TCP

For each packet from the device, before handing it to smoltcp, parse it with etherparse:

- **Not TCP or UDP:** drop.
- **UDP:** to the UDP table (below), not to smoltcp.
- **TCP SYN without ACK, 4-tuple not in the map:** create a `tcp::Socket` with rx and tx
  buffers of `tcp_buffer` bytes, apply the per-socket settings, `listen((dst_ip, dst_port))`,
  add it to the set and the map, then pass the packet on. A SYN for a 4-tuple already in the
  map just goes on (a retransmitted SYN; this avoids #1215).
- **Anything else:** to smoltcp. It answers a segment for an unknown connection with RST.

Per-socket settings: `set_congestion_control(CongestionControl::Cubic)`,
`set_nagle_enabled(false)`, `set_min_rto(config.min_rto)` (200 ms), keep the default ACK
delay to begin with, `set_keep_alive(None)`.

Hand the flow to `accept()` (an mpsc channel) once the socket reaches `Established`; check
socket states after each `iface.poll`. A socket that never gets there (the app gave up) is
removed when it is `Closed`.

### Closing

The engine relies on these, and the tests in phase 1 must pin each one:

- `poll_shutdown` → `socket.close()` (sends FIN once queued data is sent) and returns `Ready`.
  No `FinRetry`-style workaround is needed; `fin.rs` goes away with ipstack.
- **Dropping a `TcpFlow` that was not shut down resets the connection** (`socket.abort()`).
  `handle_tcp` relies on this when the upstream connect fails ("Dropping the flow resets the
  app's connection").
- Dropping a `TcpFlow` after shutdown leaves the socket in the set until smoltcp reaches
  `Closed` (after `TimeWait`), then the stack task removes it from the set and the map.
- App sends FIN → `poll_read` returns `Ok(())` with nothing read (EOF) once the buffer is
  drained.
- App sends RST → reads and writes fail with `ConnectionReset`.
- Idle timeout: close (abort) a flow with no data either way for `tcp_idle_timeout`
  (`TCP_IDLE_TIMEOUT`, one hour). Track last activity in the glue; smoltcp's own
  `set_timeout` is about unacknowledged data, not idleness.
- Dropping the `Stack` ends the task and drops every socket; flows then fail their next
  read/write. `run()`'s ordered shutdown (`drop(stack)`, listeners, flows) must still finish
  within `STOP_TIMEOUT`.

### UDP

ipstack gave one `IpStackUdpStream` per (source, destination) with an idle timeout; keep that.

- Table `HashMap<(SocketAddr src, SocketAddr dst), Entry>`; an entry holds an mpsc sender of
  datagrams to its `UdpFlow` and the last-activity time.
- A datagram for an unknown pair creates an entry and hands a `UdpFlow` to `accept()`.
- `UdpFlow::poll_read` returns one datagram; `poll_write` builds an IP+UDP packet from `dst`
  back to `src` with `etherparse::PacketBuilder` and queues it to the device.
- Expire entries after `udp_idle_timeout` (60 s): drop the sender, so the flow reads EOF,
  which ends `relay_udp` and `dns::serve_udp_flow` as today.
- Fragmented IP packets: drop (ipstack does not reassemble either).

## Phases

### Phase 0: benchmark with loss, baseline on ipstack

`tsgw/test/bench/delayed.sh` adds delay only. Add loss:

- An optional second argument or `LOSS` env, e.g. `delayed.sh 27ms 1%`, appended to the netem
  line as `loss "$LOSS"`. netem on `lo` applies once per packet, so loss hits data and ACKs.
- Run on Linux (it needs user namespaces and netem; not macOS). For each of 0%, 0.5% and 1%
  loss at 27 ms (54 ms round trip): download 200 MB and upload 100 MB, `TCP_WINDOW` 256 KiB
  and 1 MiB. Note MB/s and CPU-ms/MB as the benchmark prints them.
- Record the table in this file under **Measurements**. This is what every later phase is
  compared against.

Done when: the script takes a loss rate and the baseline table is in this file.

### Phase 1: fork smoltcp, write the glue with tests

1. Create `simophin/smoltcp` branch `cpxy` from upstream `main`. No patches yet.
2. Add `mobile-engine/src/netstack/` as above, behind the `smoltcp` feature. Nothing in
   `lib.rs` uses it yet.
3. Tests in the module, run with `cargo test -p mobile-engine --features smoltcp`. Use a
   second smoltcp `Interface` as the "app" side, wired to the `Stack` through an in-memory
   device (or a `socketpair(AF_UNIX, SOCK_SEQPACKET)` wrapped in `TunDevice`, as the existing
   `stops_when_the_other_end_of_a_socket_closes` test does). Cover at least:
   - connect to an arbitrary IPv4 and IPv6 destination; `peer_addr()` is that destination;
   - data both ways, larger than the buffer (1 MB), arrives intact and in order;
   - the SYN-ACK's MSS for MTU 1280 and 1500, IPv4 and IPv6;
   - app FIN → EOF on read; `shutdown()` → app sees FIN after all data;
   - drop without shutdown → app gets RST;
   - two connections from different source ports to the same destination both work;
   - a retransmitted SYN does not create a second socket;
   - closed sockets are removed from the set (no leak after 1000 short connections);
   - UDP: request and reply, a second pair gets its own flow, idle expiry gives EOF;
   - with `tokio::time::pause()`: idle TCP timeout aborts the flow.
4. CI: add `--features smoltcp` to the "Test mobile engine and TUN routing" job's
   `cargo test` (a second run beside the default one).

Done when: the module's tests pass locally and in CI, and the default build is unchanged.

### Phase 2: use it in the engine and tsgw; measure

1. In `lib.rs`, under `#[cfg(feature = "smoltcp")]`, build a `netstack::Stack` in `run()`
   instead of `IpStack`, and make `handle_tcp`, `handle_udp`, `relay_udp` take the new flow
   types (or `impl AsyncRead + AsyncWrite + Unpin` plus the destination). Under smoltcp,
   `handle_tcp` does not wrap the flow in `fin::FinRetry`.
2. `tsgw/Cargo.toml`: enable `mobile-engine/smoltcp`.
3. Run the whole test suite with the feature, including `mobile-engine/test/tun-lab.sh` (it
   drives the engine with the kernel's TCP on a real TUN, which is the best end-to-end check)
   and the tsgw tests in CI.
4. Benchmark as in phase 0 with buffer sizes 256 KiB, 1 MiB and 4 MiB. With congestion control,
   a large buffer should no longer hurt; pick the gateway's default from the numbers and
   update `GATEWAY_TCP_WINDOW`, the `TCP_WINDOW` row and troubleshooting text in
   [cpxy-tsgw.md](cpxy-tsgw.md).
5. Profile CPU at 0% loss. `iface.poll` walks every socket; if many idle flows make it costly,
   note it here (smoltcp has `poll_ingress_single` / `poll_egress` to narrow the work).

Done when: tsgw runs on smoltcp, CI is green, and the measurements are recorded. Expected:
at 0% loss within about 20% of ipstack's throughput and CPU; with loss, no worse than ipstack
at 256 KiB. If it is clearly worse, stop and write down why before going on.

### Phase 3: smoltcp patches in our fork

Each patch comes with tests in smoltcp's own style (unit tests in `src/socket/tcp.rs`, and the
netsim test, `tests/netsim.rs`, for throughput under loss).

1. **NewReno partial ACKs (RFC 6582).** On entering fast recovery, record `recover =
   remote_last_seq` (highest sequence sent). An ACK of new data below `recover` is a partial
   ACK: retransmit the next unacknowledged segment at once (as `pending_fast_retransmit`
   does), deflate cwnd by the newly acknowledged amount, stay in recovery. Leave recovery only
   when the ACK reaches `recover`. The congestion controllers (`reno.rs`, `cubic.rs`) today
   leave recovery on the first new ACK; give the `Controller` trait a way to know whether an
   ACK is partial (for example an `on_partial_ack` hook) instead of special-casing inside
   each controller.
2. **#1221.** Keep the highest sequence sent (`snd_max`) separately from the retransmit
   cursor that the timeout rewinds, and use `snd_max` as the SEQ of pure ACKs, as FreeBSD's
   `tcp_output.c` does. Reproduce the issue's two-peer case first as a failing test.
3. Measure again as in phase 2 and record the numbers.

Done when: both patches are in our fork with tests and the numbers are recorded.

### Phase 4: SACK

Follow upstream: #1195 (sending full SACK blocks as receiver) and the sender side (RFC 6675
scoreboard) that its author has said comes next. Review and test them against our benchmark
rather than writing a competing implementation. If upstream stalls and the phase 3 numbers
under loss are still poor, propose in this file a bounded sender-side scoreboard for our
fork, and get the plan agreed before writing it.

### Phase 5: mobile and router, remove ipstack

1. Turn the feature on for every build (Android, iOS, `mobile-engine-linux`, the router).
2. Buffer sizes per platform: buffers are allocated per socket up front (no auto-tuning,
   upstream PR #933), so memory is 2 x buffer x flows. Keep 64 KiB (`LOCAL_TCP_WINDOW`) on iOS,
   whose packet tunnel extension has a ~50 MB limit, and check memory with ~200 open flows.
   The router's local window can grow if phase 2's numbers support it.
3. Test on the Android TV emulator and the GL-MT3000 router as for any engine change.
4. Remove ipstack, `fin.rs`, the feature flag and the ipstack-specific comments in `lib.rs`
   (`WORKER_THREADS`'s reason, `TCP_IP_HEADERS_LEN`, `mss_for_mtu`, the shutdown-order
   comment). Update [mobile-vpn-plan.md](mobile-vpn-plan.md)'s "Packets are handled with the
   `ipstack` crate" decision.

## Things that will bite

- `cargo test -p mobile-engine` on macOS hangs in three TUN tests that also fail on main; skip
  `stop_closes_the_tun_descriptor`, `stops_when_the_other_end_of_a_socket_closes` and
  `reports_a_tls_connection_under_its_server_name` locally and rely on Linux CI. The
  benchmark and `tun-lab.sh` need Linux.
- smoltcp only transmits inside `iface.poll`. A flow that writes or reads and does not wake
  the stack task makes a connection that "hangs until the next packet".
- Socket buffers are fixed at creation. A tx buffer smaller than cwnd caps throughput no matter
  what congestion control does.
- The 1 s minimum RTO is the default on 0.14 and main; forgetting `set_min_rto` reproduces the
  stalls this plan is meant to remove.
- smoltcp's default `ack_delay` is 10 ms. If uploads (the app is the sender) are slow,
  compare with `set_ack_delay(None)`.
- Two smoltcp versions in `Cargo.lock` (ours from git, tailscale-rs's from crates.io) is
  expected.

## Implementation log

- Phase 1: `simophin/smoltcp:cpxy` starts at upstream main
  `24bda3b4f982625d8453d4360172557d24921c76`. The engine pins that revision without
  patching crates.io; tailscale-rs retains its own smoltcp.
- Added the optional `mobile-engine/smoltcp` module; the default engine still uses
  ipstack. Its tests use a second smoltcp interface over in-memory packet channels.
  Nine tests cover IPv4/IPv6, 1 MB transfers in each direction with 8 KiB buffers,
  per-family MSS at MTUs 1280/1500, half-close, reset/drop, tuple isolation,
  retransmitted SYNs, 1,000 graceful closes and TimeWait reaping, UDP pairs/replies/
  idle expiry, paused-time TCP expiry, shutdown wakeups and device backpressure.
- The driver polls ingress while writes are blocked. Queues between smoltcp and
  the device and per-flow UDP receive queues are bounded. Each blocked UDP writer
  has its own waker. Invalid/unconsumed SYN listeners are removed after polling,
  avoiding a stray listener intercepting an existing tuple's retransmitted SYN.
- `cargo test -p mobile-engine --features smoltcp netstack::`: 9 passed on macOS.
  Broader macOS runs (outside the sandbox, skipping the three documented hangs)
  have the same two existing failures with and without the feature:
  `relays_ipv6_unless_it_is_off` and `router_answers_dns_on_its_listen_address_until_stopped`.
  Linux CI runs both complete suites.

- Phase 2 integration: the feature selects the new stack in `run()` and bypasses
  `FinRetry`; tsgw enables it. DNS, routing, SNI and the packet filter are unchanged.
  CI adds a second real-TUN lab run with the feature. Router packaging builds the
  router and gateway separately to prevent Cargo feature unification migrating
  the router before phase 5. The 256 KiB gateway default is retained until the
  buffer-size benchmark supports a change.

## Measurements

Host: `cn-baise`, Linux 6.12.111, Rust 1.97.1, Go 1.27.1. The offline benchmark
uses unprivileged user/network namespaces and netem on WireGuard UDP only;
27 ms one-way delay, 200 MB download, 100 MB upload, one transfer per direction
per configuration. No live tailnet credentials are needed for this benchmark.
CPU is gateway CPU-ms/MB, including its engine and Tailscale threads.

| Stack | Buffer | Loss | Download MB/s | CPU-ms/MB | Upload MB/s | CPU-ms/MB |
|---|---:|---:|---:|---:|---:|---:|
| ipstack | 256 KiB | 0% | 4.7 | 70.7 | 4.6 | 125.6 |

Further phase 0/2/3 measurements will be recorded here as they finish.
