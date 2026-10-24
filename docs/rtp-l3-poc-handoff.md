# RTP/UDP L3 tunnel POC handoff

## Objective and scope

Build and measure an experimental IPv4 packet tunnel carried in RTP-framed UDP.
The immediate questions are throughput, loss, latency and observable packet shape
on the authorized test path. This does not establish resistance to censorship.
No desktop/mobile integration, DNS splitting, production rollout, or changes to
the existing proxy are requested. Keep the experiment in a standalone Rust crate.

Branch: `poc/rtp-l3-tunnel`, created from local `main` / `origin/main` at
`e112599400ec1b9c9705a136f5e41b458dfc104e`. The first commit is this handoff.

## Public-repository hygiene

Never put preshared keys, credentials, SSH configuration, actual hostnames,
usernames, endpoint addresses, or private deployment paths in tracked files,
captures, logs, command examples, or this document. Use CLIENT_HOST and
SERVER_HOST placeholders. Obtain actual access details from the active user
conversation, not the public repository. Keys must be generated outside the
checkout, transferred privately, read from restricted files, and never printed.
Keep raw captures and potentially identifying measurement artifacts outside Git;
commit sanitized aggregates only. Do not commit a key even for convenience.

## Authorized environment and constraints

- Client is an unprivileged Linux x86_64 account. No sudo or host route/sysctl
  changes. Verified: user/network namespaces work, `/dev/net/tun` is accessible,
  `ip`, `unshare`, and `iperf3` are available. Remote login shell requires wrapping
  commands in `sh -c` or `sh -lc`. Existing services are unrelated and must remain
  untouched.
- Server is an authorized root-access Linux x86_64 host with TUN, iperf3, and
  tcpdump. It is public-facing. Use **firewalld runtime rules only** to open any
  required ports temporarily; never use permanent rules or bypass firewalld.
  Record exact changes and remove task-owned openings after testing, including
  on failures. Inspect existing networking before choosing nonconflicting ports,
  interface names, address ranges, or forwarding rules.
- Client namespace should contain TUN, routes and benchmark applications. Create
  the outer UDP socket in the original network namespace and inherit it into the
  isolated namespace, or use an equally narrow unprivileged arrangement. Verify
  this mechanism before investing in the full tunnel.
- Server creates a dedicated temporary TUN and narrow forwarding/NAT setup.
  Isolate settings where practical; restore any changed host settings. Add
  idempotent cleanup for processes, interfaces, runtime firewall rules, and files.
- GitHub identity, if GitHub operations become necessary: inspect repository
  `.envrc` safely, explicitly apply its personal `GH_CONFIG_DIR`, verify login is
  `simophin` before writes, and use the same identity for Git credentials. Never
  print credentials. No GitHub publishing is required by this task.

## Proposed implementation

1. Standalone Linux Rust binary with server/client modes, plus scripts for setup,
   cleanup, captures and repeatable benchmarks. Reuse existing dependencies where
   sensible; do not change existing production behavior.
2. IPv4 initially; one inner IP packet per UDP datagram, inner MTU 1200 bytes.
   No outer reliable ordering/retransmission, FEC, or fragmentation. Endpoint TCP
   stacks retain responsibility for TCP recovery. Bound queues and report drops.
3. Standard 12-byte RTP header: stable SSRC per session, increasing 16-bit
   sequence, monotonic-clock 90 kHz timestamps. Clearly label this as RTP-framed
   encrypted IP, not valid codec media or a demonstrated camouflage protocol.
   First measure natural traffic; defer media pacing/padding and full RTCP.
4. Static 256-bit PSK and ChaCha20-Poly1305. Retain the 16-byte authentication
   tag: it protects replay state and server forwarding, regardless of inner TLS.
   Authenticate visible RTP headers as associated data. Use distinct directional
   traffic keys and unique per-packet nonces. No certificates, DH or forward secrecy.
5. Small PSK-authenticated session establishment with fresh session material on
   every restart. Prevent old control exchanges from recreating old sessions.
   Do not derive restart safety solely from a random 32-bit SSRC. Specify the
   transcript, key derivation and nonce rules before implementing them.
6. SRTP-style extended packet index: RTP sequence plus implicit rollover counter;
   no added per-data-packet counter. Sliding replay bitmap tolerates reordering.
   Authenticate before advancing state. Handle rollover, bounded index ambiguity,
   large loss gaps, and re-establishment explicitly; never reuse a key/nonce pair.
7. Expected data overhead over IPv4 is 56 bytes (20 IP + 8 UDP + 12 RTP + 16 tag).
   Session setup overhead is separate. Include runtime counters and machine-readable
   measurement output. Do not log keys or decrypted application content.

References: RFC 3550 (RTP), RFC 3711 section 3.3 (implicit packet index/replay),
RFC 8439 (ChaCha20-Poly1305), RFC 8085 (UDP tunnel congestion considerations),
RFC 8899 (datagram MTU). This is a custom experimental tunnel, not standardized SRTP.

## Checkpoints and commit discipline

The implementing agent owns code, tests, remote deployment, cleanup, measurements,
and updates to this document. Commit each coherent completed checkpoint with its
handoff updates. Do not wait until the entire experiment is finished. Before any
interruption, update Current state with exact next actions and cleanup obligations.
Keep this document sufficient for another agent to resume without replaying the
full conversation; access details remain exclusively in private session context.

- [x] P0: inspect local baseline and remote prerequisites; commit handoff first.
- [ ] P1: prove unprivileged namespace/socket/TUN path; specify wire/crypto lifecycle.
- [ ] P2: implement tunnel and focused correctness tests; commit.
- [ ] P3: local/namespace integration, rollover/restart and loss/reordering checks; commit.
- [ ] P4: deploy temporary server/client, validate forwarded traffic and capture shape.
- [ ] P5: complete bounded measurement matrix; commit sanitized report and commands.
- [ ] P6: remove remote processes, runtime firewall openings and network changes;
      verify cleanup; commit final handoff status.

## Validation and measurement plan

Correctness: packet round trips, malformed input, authenticated header/ciphertext
tampering, replay/duplicates, allowed reordering, sequence rollover, session
restart, stale control traffic, nonce uniqueness, MTU and graceful cleanup.
Verify namespace ping, TCP/UDP transfers and server egress forwarding. Ensure
the namespace uses kernel TCP, not the existing userspace TCP implementation.

Measure sequentially (no compilation or overlapping throughput runs):

- Matched direct TCP/UDP baseline between the same hosts; do not expose a public
  unrestricted benchmark service. Tunnel-only benchmark listeners are preferred;
  any necessary public baseline listener requires a narrow temporary runtime rule.
- Upload/download TCP with one and four streams, approximately 20–30 seconds,
  three repetitions per case where feasible. Record congestion algorithm/buffers.
- Paced UDP rate ladder in both directions; increase cautiously, stop escalating
  on substantial loss. Record offered rate, delivered rate, loss, duplicates,
  reordering, jitter and local socket/queue drops; distinguish final loss from
  temporary sequence gaps.
- Idle and loaded RTT median/p95/p99. Do not claim one-way delay without suitable
  clock synchronization. Record CPU, memory, payload and wire byte counts.
- Filtered outer packet captures: packet size distribution, interarrival times,
  bursts, RTP sequence/timestamp progression. Keep raw endpoint-bearing artifacts
  private. Note capture drops/offload effects before interpreting loss or shape.
- Record UTC test windows and limitations. A short run does not establish peak-hour
  QoS behavior; repeat at peak/off-peak later if needed. Do not infer censorship
  resistance from throughput or Wireshark's protocol label.

## Current state

Handoff prepared before implementation. Remote prerequisite inspection only;
no deployed task processes, opened ports, created TUN devices or persistent network
changes. The namespace prerequisite check was temporary and exited successfully.

Next: implementing subagent reads this file, validates the inherited UDP socket
and unprivileged TUN arrangement, then records wire format/session design and
implements P1/P2. Use only the private conversation for actual remote access.

## Progress log

- 2026-10-24: created branch from main; verified Linux prerequisites on both hosts.
  User requires temporary firewalld openings on the public server and cleanup.
