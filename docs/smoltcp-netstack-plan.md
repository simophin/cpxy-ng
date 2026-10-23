# Replacing ipstack with smoltcp: plan

Status: Phases 0–4 are implemented and tested, including the approved bounded
sender SACK and the recovery-entry correction in fork `d64e86d`. **The
performance acceptance gate remains unmet.** The corrected 256 KiB build moves
4.1/4.5 MB/s on the clean download/upload, but only 0.9 MB/s on the 0.5% loss
download against ipstack's 2.3 MB/s. Per the stop rule, the corrected matrix is
not expanded after that failure. Earlier revisions' full-size 1% downloads and
lossy uploads also miss their deadlines; short diagnostics are recorded
separately. Keep the 256 KiB gateway default. Continuation details are in
[the handoff](smoltcp-netstack-handoff.md); Phase 5 is outside this request.

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

- Linux verification: all 60 feature-enabled engine library tests and 51 default
  engine tests pass, plus the binary tests. The real-TUN lab passes, including
  16 parallel 4 MB transfers. The existing shutdown test now completes the TCP
  handshake before waiting for the upstream connection; smoltcp accepts only
  established connections. The DNS listener fixture uses IPv6-off mode, matching
  its assertion that AAAA is answered without a host DNS server.
- Phase 3: fork commits `da00976` (NewReno) and `8774bfc` (#1221) pass 711
  smoltcp unit tests and all three netsim tests. The partial-ACK tests fail on
  upstream for both Reno/CUBIC and cover sequence wrap; the two-peer ACK-loss
  reproduction stalls at 2048/8192 bytes per peer before the fix. Regressions
  also cover the full-window pure-ACK boundary and preserving the RTO cursor.
  The original netsim snapshots are explicitly kept on no congestion controller;
  a dedicated CUBIC/200 ms RTO test covers 0/0.5/1% loss at 54 ms RTT.
- Standalone fork branches `cpxy-newreno` and `cpxy-rto-ack` isolate the fixes for
  human review. Upstream's [contributing policy](https://github.com/smoltcp-rs/smoltcp/blob/main/CONTRIBUTING.txt)
  requires human-authored code submissions, so no upstream PR containing this
  generated code was opened.
- Phase 4: upstream #1195 is open. Its author JPDye has no public sender-SACK
  branch or PR as of 2026-10-23. Receiver work has been cherry-picked through
  `2f038a3` (preserving original authorship); the reset adaptation preserves
  `set_min_rto`. The engine pins `01bf32a` after 726 unit tests, all three
  netsim tests, and ten packet-level engine tests pass. Simultaneous 1 MB transfers
  and half-closes pass for both IPv4 and IPv6. At that checkpoint, sender-side
  RFC 6675 was still pending; the approved fallback follows below.
- Phase 4, sender side: fork commit `478752b` implements the approved bounded
  RFC 6675 recovery in `src/socket/tcp/sack.rs` and `src/socket/tcp.rs`. Each
  socket keeps at most 32 sorted, merged SACKed ranges (264 bytes) plus the
  `HighRxt` and `RescueRxt` marks; a full scoreboard forgets the ranges farthest
  from the cumulative ACK. In recovery with SACK negotiated, `IsLost`, `SetPipe`
  and `NextSeg` replace NewReno: lost holes are retransmitted first, then new
  data, then not-yet-lost holes below the highest SACKed octet, then one rescue
  of an unretransmitted tail per episode. It suppresses further duplicate-ACK
  inflation and partial-ACK deflation until the recovery point is acknowledged;
  the initial three-MSS allowance was corrected subsequently in `d64e86d`.
  Blocks are validated against the negotiated capability and `[SND.UNA, SND.MAX]`
  with wrap-safe comparisons; a timeout clears the scoreboard and the cursor
  resends the flight, cutting segments at ranges reported after the timeout.
  Retransmissions never exceed the advertised window. Two departures from the
  RFC text avoid duplicate retransmissions seen in simulation: rule 4 rescues
  only data not already retransmitted in the episode, and `HighRxt` survives the
  end of an episode. Thirteen new unit tests cover the proposal's list (multiple
  holes before the timer, no resend of SACKed data, loss detection before three
  duplicates, reordering, duplicate/overlapping/invalid blocks, sequence wrap,
  scoreboard overflow, reneging and timeouts, zero windows, the rescued tail,
  peers without SACK, simultaneous bidirectional loss); the fork passes 742 tests
  in the default CI feature set, the IPv6-only and no-std sets, and all three
  netsim cases. The `netsim` snapshots change because the no-controller flows now
  recover with SACK: single-flow throughput under random loss rises (for example
  32 KiB buffers at 5% loss, 101 to 150 KB/s), while the no-controller multi-flow
  case, which has no congestion window to bound RFC 6675's retransmissions, gets
  more aggressive (higher aggregate, more tail drops, lower fairness). The
  engine pins `478752b`; its ten packet-level tests, the native Linux suites (61
  with the feature, 51 default) and the real-TUN lab pass.
- Continuation: fork `d64e86d` corrects recovery entry to use `cwnd = ssthresh`
  for negotiated SACK, per [RFC 6675 section 5 step 4](https://www.rfc-editor.org/rfc/rfc6675.html#section-5). `478752b` suppressed
  subsequent duplicate-ACK inflation but inadvertently retained NewReno's initial
  three-MSS allowance; its earlier description of cwnd staying at ssthresh was
  inaccurate. A separate controller hook preserves the existing NewReno path
  for peers without SACK. Reno/CUBIC packet regressions fail against the previous
  entry behavior, including sequence wrap; pipe-budget and zero-window fixtures
  now reflect the reduced budget. All 742 fork unit tests, three netsim cases,
  formatting and the no-std configuration pass. The engine pins `d64e86d`.

## Measurements

Host: `cn-baise`, Linux 6.12.111, Rust 1.97.1, Go 1.27.1. The offline benchmark
uses unprivileged user/network namespaces and netem on WireGuard UDP only;
27 ms one-way delay, 200 MB download, 100 MB upload, one transfer per direction
per configuration. No live tailnet credentials are needed for this benchmark.
CPU is gateway CPU-ms/MB, including its engine and Tailscale threads.

| Stack | Buffer | Loss | Download MB/s | CPU-ms/MB | Upload MB/s | CPU-ms/MB |
|---|---:|---:|---:|---:|---:|---:|
| ipstack | 256 KiB | 0% | 4.7 | 70.7 | 4.6 | 125.6 |
| ipstack | 256 KiB | 0.5% | 2.3 | 478.4 | timeout | — |
| ipstack | 256 KiB | 1% | 1.4 | 810.3 | timeout | — |
| ipstack | 1 MiB | 0% | 16.5 | 59.1 | 5.1 | 116.2 |
| ipstack | 1 MiB | 0.5% | 1.6 | 1002.6 | timeout | — |
| ipstack | 1 MiB | 1% | 1.5 | 1035.6 | timeout | — |
| smoltcp Phase 2 (`24bda3b`) | 256 KiB | 0% | 4.2 | 46.3 | 4.5 | 100.0 |
| smoltcp Phase 2 (`24bda3b`) | 1 MiB | 0% | 6.7 | 41.5 | 5.2 | 96.0 |
| smoltcp Phase 2 (`24bda3b`) | 4 MiB | 0% | 7.8 | 38.3 | timeout | — |
| smoltcp Phase 2 (`24bda3b`) | 256 KiB | 0.5% | timeout: 191.4/200 MB in 300 s | — | not repeated | — |
| smoltcp Phase 3 (`8774bfc`) | 256 KiB | 0% | 4.2 | 47.7 | 4.5 | 98.9 |
| smoltcp Phase 3 (`8774bfc`) | 1 MiB | 0% | 10.6 | 33.5 | 6.2 | 91.5 |
| smoltcp Phase 3 (`8774bfc`) | 4 MiB | 0% | 8.7 | 36.5 | timeout | — |
| smoltcp Phase 3 (`8774bfc`) | 256 KiB | 0.5% | timeout: 186.4/200 MB in 300 s | — | not repeated | — |
| smoltcp sender SACK (`478752b`) | 256 KiB | 0% | 4.2 | 48.0 | 4.5 | 99.9 |
| smoltcp sender SACK (`478752b`) | 1 MiB | 0% | 8.4 | 39.0 | 5.2 | 93.1 |
| smoltcp sender SACK (`478752b`) | 256 KiB | 0.5% | 1.2 | 119.2 | timeout: 53.6/100 MB in 120 s | — |
| smoltcp sender SACK (`478752b`) | 1 MiB | 0.5% | 1.5 | 96.2 | timeout: 55.9/100 MB in 120 s | — |
| smoltcp sender SACK (`478752b`) | 256 KiB | 1% | timeout: 140.2/200 MB in 300 s | — | timeout: 39.2/100 MB in 120 s | — |
| smoltcp sender SACK (`478752b`) | 1 MiB | 1% | timeout: 134.7/200 MB in 300 s | — | timeout: 41.7/100 MB in 120 s | — |
| smoltcp SACK entry fix (`d64e86d`) | 256 KiB | 0% | 4.1 | 49.5 | 4.5 | 101.0 |
| smoltcp SACK entry fix (`d64e86d`) | 256 KiB | 0.5% | 0.9 | 141.8 | not repeated | — |

Lossy baseline uploads timed out at 100 MB. A timeout alone does not distinguish
slow congestion-controlled delivery from the known gateway receive-path stall;
these attempts are censored measurements, not completed throughput results.
The benchmark now bounds a blocked userspace-TCP write by closing the connection, bounds gateway cleanup,
and can isolate transfer directions. Later reruns use a 120 s upload limit;
downloads retain 300 s. One old hard timeout left a gateway process alive;
the next 1 MiB/0% result was discarded and rerun after reaping it. Control-plane
startup failures are not throughput results and are retried separately.

### Phase 2 gate checkpoint

At 256 KiB and zero injected loss, throughput is within the 20% gate and CPU is
lower than ipstack. The 1 MiB clean-path download is **59% slower** than ipstack
at the same buffer size (6.7 vs 16.5 MB/s), despite lower CPU cost. Four MiB gives
only another 16% download throughput and the 100 MB upload times out. A larger
buffer alone is therefore not a sufficient fix, and the gateway default remains
256 KiB pending the recovery-patched measurements.

The 256 KiB/0.5% download also fails the gate: 191,383,720 bytes arrive before
its 300 s deadline (about 0.64 MB/s during that interval), versus ipstack's
completed 2.3 MB/s transfer. This is a slow transfer, not evidence that it was
permanently stalled. Per the Phase 2 stop rule, the remaining unpatched loss
matrix is not expanded; compare the recovery build next to diagnose the gap.

Do not treat the integration as having passed the performance gate. The saved
unpatched/recovery/SACK binaries allow diagnosis without erasing the Phase 2
comparison. Investigate loss recovery and the congestion window before changing
the default or recommending rollout. The loss-only netem fixture has no bandwidth
bottleneck: CUBIC reduces its congestion window on loss while fixed-window ipstack
does not. That difference must be distinguished from retransmission stalls; see
[RFC 9438 §4.6](https://www.rfc-editor.org/rfc/rfc9438.html#section-4.6).

The recovery build improves the clean 1 MiB download to 10.6 MB/s and reduces
CPU to 33.5 CPU-ms/MB, but the 0.5% loss gate still fails: 186,435,425 of
200,000,000 bytes arrive before the 300 s watchdog closes the connection (about
0.62 MB/s over that interval). This result does not show a loss-throughput gain
over unpatched smoltcp. The remaining recovery loss matrix is not expanded after
this failure. Four MiB is slower than 1 MiB on the clean download and its upload
also times out. Keep the 256 KiB default and do not recommend rollout while this
gate is unmet. Receiver SACK passes the native engine suite and TUN lab.

### Sender SACK gate checkpoint

Sender SACK (`478752b`, measured 2026-10-24 with the same fixture) completes
the 0.5% downloads that every earlier smoltcp build censored: 1.2 MB/s at
256 KiB and 1.5 MB/s at 1 MiB, about double the ~0.62 MB/s the recovery build
managed before its deadline, and the 1 MiB figure is within 10% of ipstack's
1.6 MB/s at that buffer. Zero-loss numbers are unchanged within noise (the
1 MiB download measured 8.4 MB/s against 10.6 MB/s for the recovery build on a
single run each). The gate is still unmet: ipstack's uncontrolled 256 KiB window
moves 2.3 MB/s at 0.5%, the 1% downloads deliver about 0.47 MB/s and miss the
300 s deadline (ipstack completed at 1.4 MB/s), and every lossy upload, where
Tailscale's gVisor CUBIC stack is the sender and smoltcp the receiver, moves
0.33–0.47 MB/s and misses its 120 s deadline (ipstack's lossy uploads also timed
out). Keep the 256 KiB default and do not recommend rollout.

### Bounded sender-SACK proposal (approved 2026-10-23, implemented in `478752b`)

Approved by the user after Phase 3 missed the loss gate and upstream sender
work remained unpublished; implemented as described, see the implementation
log for the two deliberate departures from the RFC text.

- A fixed-capacity scoreboard of 32 sorted, merged sequence ranges per socket,
  plus bounded retransmission bookkeeping (under 1 KiB of socket metadata).
  Keep payload in the existing transmit buffer until cumulatively acknowledged.
- Validate received blocks against the negotiated SACK capability and the
  `[SND.UNA, SND.MAX]` interval, using wrap-safe sequence comparisons. Ignore
  DSACK/below-ACK blocks, malformed ranges and claims for unsent data.
- Implement RFC 6675 `IsLost`, `SetPipe` and `NextSeg` with `HighACK`, `HighData`,
  `HighRxt`, `RescueRxt` and the existing recovery boundary. Use the existing
  congestion-controller hooks; do not grow cwnd on partial recovery ACKs.
- If the scoreboard fills, retain the ranges nearest the cumulative ACK and
  conservatively forget farther SACK information. This can retransmit extra data
  but cannot free unacknowledged bytes or prevent recovery. Clear SACK state on
  RTO to handle receiver reneging; retain the tested NewReno fallback when SACK
  was not negotiated or recovery information is insufficient.
- Deterministic tests must cover multiple holes repaired before RTO, avoiding
  retransmission of SACKed data, reordering, overlapping/duplicate blocks,
  sequence wrap, bounded-capacity overflow, reneging/RTO, zero/full windows,
  simultaneous bidirectional loss, and peers without SACK negotiation.
- Re-run the real-TUN lab and the same buffer/loss matrix, measuring CPU and
  memory as well as throughput. Adopt only after meeting the recorded gate.

### Sender SACK in simulation: faster repair, not more throughput

The fork's `478752b` netsim (54 ms RTT, 25 MB/s bottleneck, 256 KiB buffers, random
loss on data and ACKs, CUBIC with 200 ms minimum RTO) runs each case with SACK
negotiated (RFC 6675) and without (NewReno), on the same seeds, 10 MB per run:

| Loss | Seed | NewReno KB/s | SACK KB/s | NewReno episodes / RTOs | SACK episodes / RTOs |
|---:|---:|---:|---:|---:|---:|
| 0.5% | 1 | 482 | 478 | 40 / 0 | 39 / 0 |
| 0.5% | 2 | 437 | 369 | 48 / 0 | 46 / 1 |
| 0.5% | 3 | 542 | 459 | 41 / 1 | 43 / 0 |
| 0.5% | 4 | 362 | 356 | 48 / 0 | 49 / 0 |
| 0.5% | 5 | 458 | 435 | 41 / 0 | 38 / 0 |
| 0.5% | 6 | 613 | 503 | 34 / 0 | 38 / 0 |
| 1% | 1 | 442 | 353 | 59 / 0 | 75 / 1 |
| 1% | 2 | 340 | 313 | 68 / 1 | 71 / 2 |
| 1% | 3 | 285 | 253 | 90 / 3 | 99 / 1 |
| 1% | 4 | 302 | 280 | 83 / 1 | 86 / 1 |
| 1% | 5 | 269 | 271 | 87 / 0 | 88 / 1 |
| 1% | 6 | 275 | 269 | 83 / 1 | 88 / 1 |

Reproduce with `NETSIM_LOSS=0.01 NETSIM_SEED=2 cargo test --release --features
_netsim,socket-tcp-cubic --test netsim netsim_cubic_loss_exploration --
--ignored --nocapture` in the fork (`NETSIM_BYTES`, `NETSIM_RTT_MS`,
`NETSIM_BUFFER` and `NETSIM_SACK=0|1` also apply). These numeric samples predate
the SACK recovery-entry correction in `d64e86d`.

The deterministic packet tests repair multiple holes within the same round
trip without waiting for RTO. In these simulation samples, SACK does not raise
CUBIC throughput relative to NewReno, and the table does not show a consistent
increase in timeout frequency either. The saved traces attribute part of the
throughput difference to recovery duration and window accounting: NewReno's
duplicate-ACK inflation keeps more data in flight during longer episodes. That
inflation is allowed for NewReno under RFC 5681; RFC 6675 uses SetPipe instead.
This comparison does not make NewReno noncompliant or establish the complete
cause of the gateway's deficit. In particular, the later real-gateway traces
also identify retransmission timeouts. Keep the RFC 6675 entry-window correction
and the existing NewReno fallback rather than changing congestion policy to
improve one random-loss fixture.

One remaining inefficiency in these traces is that a rescue or a new episode's
first retransmission can resend a segment whose repair is still in flight (about 12
to 16 duplicate segments per 10 MB at 1% loss, each answered by a challenge
ACK). That costs well under 1% of the transfer and is left as is.

### Continuation: kernel reference and timeout diagnostics (2026-10-24)

`tsgw/test/bench/kernel.sh` runs kernel CUBIC in an unprivileged user/network
namespace. It sets loopback MTU 1280, disables TSO/GSO/GRO before applying netem
to all loopback packets, and records iperf3 JSON plus offload/qdisc diagnostics.
Each sample below lasts 30 seconds at 54 ms RTT, with a 256 KiB socket-buffer
request. Linux reports actual send/receive buffer sizes of 425,984 bytes.
This isolates TCP's reaction to loss; it does not reproduce the gateway's
WireGuard encryption, relay, gVisor peer or fixed memory allocation.

| Loss | Reverse MB/s | Forward MB/s | Reverse retransmits | Forward retransmits |
|---:|---:|---:|---:|---:|
| 0% | 3.801 | 3.820 | 0 | 0 |
| 0.5% | 0.716 | 0.763 | 88 | 104 |
| 1% | 0.376 | 0.366 | 127 | 80 |

The 1 MiB request is unsupported: iperf3 reports `socket buffer size not set
correctly`. The host's `rmem_max`/`wmem_max` are 212,992 bytes and cannot be raised
from an unprivileged network namespace. The host settings were left unchanged.
Reproduce with `tsgw/test/bench/kernel.sh 27ms 0.5% 262144 30 -R`; raw results
are `measurements/kernel-*.json` and `kernel-*.log` on the Linux workspace.
The first 10-second probe had aggregation enabled and is excluded from this table.

These samples show a substantial loss penalty for kernel CUBIC too. They do
not establish a throughput ceiling for all compliant stacks or waive the
existing comparison with ipstack. The performance gate remains unmet.

The fork's `log` feature now routes stack diagnostics through the gateway's
existing subscriber. With `CPXY_TSGW_BENCH_GW_RUST_LOG=warn,smoltcp::socket::tcp=debug`
and `CPXY_TSGW_BENCH_LOG`, four short 20 MB measurements on `478752b` completed:

| Loss | Download MB/s | Upload MB/s | Download RTO messages | Upload RTO messages |
|---:|---:|---:|---:|---:|
| 0.5% | 2.3 | 0.6 | 2 | 1 |
| 1% | 0.4 | 0.3 | 13 | 1 |

Counts include warm-up/control flows, so they are diagnostic counts, not an
exact per-transfer loss rate. None of the four gateway logs contains an
`assembler: too many holes` message. The four-range receiver limit therefore
does not explain these upload samples; it was not increased. All short uploads
finish, which distinguishes these samples from a permanent upload stall but
does not rule out the separate long-running receive-path problem. The upload
sender is the test client's **gVisor** stack, configured to CUBIC by
`tailscale.com/wgengine/netstack`, rather than Linux TCP. Logging overhead,
random losses and the shorter size prevent treating these as replacements for
the full-size matrix. Raw logs are `measurements/debug4-*`.

The 1% download does encounter retransmission timeouts, so attributing its
entire deficit to CUBIC's congestion response was too strong. Distinguishing
lost tail packets, lost repairs and ACK loss needs packet traces; no RACK/TLP
or congestion-policy changes are included in Phases 1–4.


### CPU profile: established idle connections

On `cn-baise`, release mode, recovery revision `8774bfc`, 100,000 polls per
sample. Connections are established through the packet-level fixture with
8 KiB buffers; no live network or root permissions are involved.

| Established idle sockets | `iface.poll` µs/call | Full glue poll µs/call |
|---:|---:|---:|
| 1 | 0.036 | 0.177 |
| 100 | 2.757 | 7.630 |
| 1,000 | 27.923 | 80.217 |

Both paths scale linearly with socket count. With 1,000 idle sockets, polling
on every packet is costly: these polls alone can consume a core at roughly
12,500 polls/s before relay or Tailscale work, and the driver may poll more than
once per packet. The extra glue cost includes idle-deadline scans and reaping;
future optimisation should cache deadlines/reap less often and evaluate
`poll_ingress_single`/targeted egress before migrating large router workloads.
Reproduce with `cargo test --release -p mobile-engine --features smoltcp --lib
profile_idle_socket_polling -- --ignored --nocapture`.


### Final validation and handoff

- The receiver-SACK fork at `01bf32a` passes 726 unit tests and all three netsim
  tests. Its native Linux engine run passes 61 tests, with the manual profiler
  ignored; the real-TUN lab passes again. `go vet ./...` passes.
- A no-delay 20 MB gateway smoke test passes at about 74 MB/s in both directions.
  This is a smoke test, not a result comparable to the 54 ms/200 MB table above.
- The subsequent native `TestStateFromGo` fails because the gateway exits with
  `Address already in use`. Its cause is not established; rerun it in an isolated
  namespace before attributing it to the stack. All CI checks passed on commit
  `4b53f15` (run `39952887957`), including the gateway/state-import job.
- A ten-second `perf` capture of the recovery gateway at zero injected loss uses
  `cpu-clock:u`, 99 Hz, DWARF stacks and regular user permissions. It contains 56
  samples and no lost samples. Symbols include Poly1305/ChaCha20, X25519, Tokio
  async I/O and Tailscale actor/control code. The sample is too small for precise
  hotspot percentages; the idle-socket measurements above provide stronger
  evidence of scaling cost.
- The final debug build filled the host filesystem. Only this task's disposable
  `target/debug` cache was cleared (about 4.8 GiB); release artifacts and measured
  binaries were preserved. Final native tests were rerun in release mode.


### Corrected SACK recovery entry: final validation (`d64e86d`)

The fork passes 742 unit tests, all three netsim cases, the no-std check and
formatting. The Reno/CUBIC entry-window regression checks both fail with the
old hook and pass with the correction. Native Linux engine validation passes
61 tests (one manual profiler ignored); the real-TUN lab passes again, including
16 parallel 4 MB downloads, IPv4/IPv6 routing, DNS split, refusals, fail-closed
behavior and ordered shutdown.

`TestStateFromGo` passes in a fresh user/network namespace with the corrected
binary. The first no-delay 20 MB smoke attempt downloads at 67.2 MB/s but its
upload stalls and reaches Go's four-minute hard deadline before the original
300-second transfer watchdog. The task-owned orphan gateway was terminated;
no host service was changed. The upload retry uses a 30-second transfer deadline
and passes at 74.7 MB/s. Both attempts are preserved as `sack-window-smoke.log`
and `sack-window-smoke-retry.log`. This is not evidence that the separate,
known receive-path stall has been fixed.

CI on code commit `80f24e6` passes every applicable check, including the gateway,
workspace tests, both engine/TUN configurations, mobile shared UI, packaging
and both Docker architectures: [run 39987822560](https://github.com/simophin/cpxy-ng/actions/runs/39987822560).

The final full-size gate samples use the same 54 ms RTT, 256 KiB buffer,
200 MB download / 100 MB upload fixture, with debug logging disabled:

| Loss | Direction | MB/s | CPU-ms/MB | Peak RSS MB |
|---:|---|---:|---:|---:|
| 0% | Download | 4.1 | 49.5 | 27.3 |
| 0% | Upload | 4.5 | 101.0 | 25.3 |
| 0.5% | Download | 0.9 | 141.8 | 27.0 |

The 0.5% download completes in roughly 211 seconds of measured transfer time
(217 seconds including test setup), but remains clearly worse than ipstack's
2.3 MB/s. It also falls below the older `478752b` sample's 1.2 MB/s; one random
sample per revision cannot isolate the cause of that difference. The recovery
entry correction is for RFC 6675 correctness, not a demonstrated throughput
improvement. The zero-loss results meet the 20% throughput gate and use less
CPU than ipstack at the same buffer size.

Per the Phase 2 stop rule, do not expand the corrected pin's larger-buffer/loss
matrix after this failure. Kernel references show that random loss penalizes
kernel CUBIC too, and gateway traces identify retransmission timeouts; neither
proves the whole deficit is unavoidable. No congestion-policy change, receiver
capacity increase or default-buffer change is justified by these measurements.
The implementation and its validation are present; performance acceptance and
rollout remain blocked by the recorded gate. Raw logs are
`measurements/sack-window-262144-*`; no task-owned build, gateway or benchmark
process remains running.
