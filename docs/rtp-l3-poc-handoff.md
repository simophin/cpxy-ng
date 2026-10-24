# RTP/UDP L3 tunnel POC handoff

## Objective and scope

Build and measure an experimental IPv4 packet tunnel carried in RTP-framed UDP.
The immediate questions are throughput, loss, latency and observable packet shape
on the authorized test path. This does not establish resistance to censorship.
No desktop/mobile integration, DNS splitting, production rollout, or changes to
the existing proxy are requested. Keep the experiment in a standalone Rust crate.

Branch: `poc/rtp-l3-tunnel`, created from local `main` / `origin/main` at
`e112599400ec1b9c9705a136f5e41b458dfc104e`. The first commit is this handoff.

The v1 experiment is complete at `4e1ab87`. The user explicitly started the
v2 XChaCha20 experiment below on 2026-10-24. Its revised crypto requirements
supersede the original v1 handshake/authentication/replay requirements for v2.

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

## Original implementation (v1, completed)

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
- [x] P1: prove unprivileged namespace/socket/TUN path; specify wire/crypto lifecycle.
- [x] P2: implement tunnel and focused correctness tests; commit.
- [x] P3: local/namespace integration, rollover/restart and loss/reordering checks; commit.
- [x] P4: deploy temporary server/client, validate forwarded traffic and capture shape.
- [x] P5: complete bounded measurement matrix; commit sanitized report and commands.
- [x] P6: remove remote processes, runtime firewall openings and network changes;
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

## Wire and session specification (v1)

Control messages use magic `R3P1`, one-byte type, client random C (32 bytes),
server random S (32 bytes except HELLO), and HMAC-SHA256 over the full preceding
message prefixed with `rtp-l3-poc/control/v1`. Types are HELLO, CHALLENGE, FINISH,
READY. Every HELLO is authenticated; fresh server randomness prevents a recorded
exchange recreating old keys after restart. Duplicate HELLO reuses an unexpired
pending challenge. FINISH must authenticate and match the pending peer/transcript
before installation; duplicate FINISH for the installed session resends READY
without resetting any counters. Pending challenges expire after 10 seconds.
Existing sessions remain installed until a valid replacement FINISH.

HKDF-SHA256 uses C||S as salt and the PSK as input, with separate versioned
client-to-server and server-to-client labels to derive 32-byte traffic keys and
32-bit SSRCs. Data uses a fixed 12-byte RTP v2 header, payload type 96, initial
sequence zero, monotonic elapsed-time 90 kHz timestamps. ChaCha20-Poly1305 takes
the complete header as AAD. Its nonce is four zero bytes followed by the 64-bit
extended packet index. Sequence carries its low 16 bits; no extra index is sent.
Keys are independent by direction and each fresh handshake. Transmit stops before
index 2^48. Receivers infer the nearest index, supporting forward gaps below
32768 packets and a 128-packet replay window; larger outages require restart.
Authentication succeeds before replay, session or peer state changes. A fresh
session rejects stale data. The custom control exchange is observable; this is
not codec-valid media or standardized SRTP.

## Next experiment: handshake-free XChaCha20 (v2, COMPLETE)

### Objective and accepted threat model

The user's objective is censorship circumvention through traffic concealment,
not an additional layer of application security. Encrypt the entire inner IP
packet, including IP headers and TLS handshake/SNI, while preserving the visible
RTP framing. Existing end-to-end TLS continues to protect TLS application data.
For this experiment the user explicitly accepts **no Poly1305/tag, no tunnel
authenticity, no replay protection and no forward secrecy**. Do not reintroduce
a session handshake or replay machinery to supply those excluded properties.
This scope choice does not make nonce reuse safe: concealment still requires
avoiding repeated keystreams under the same key.

The v1 `R3P1` HELLO/CHALLENGE/FINISH/READY exchange is a directly recognizable
startup signature. V2 must send only data packets with RTP framing, including
from the first packet; no custom magic, setup datagrams, challenge exchange,
wire negotiation, session identifier exchange, or fallback probe to v1.

### Packet format and crypto

```text
outer IPv4 | UDP | RTP header (12) | random nonce (24) | encrypted inner IP
```

- Use **raw XChaCha20**, not ChaCha20 and not XChaCha20-Poly1305. Its nonce is
  192 bits / 24 bytes and its key is 256 bits / 32 bytes. Generate a fresh full
  24-byte nonce with an OS-backed cryptographically secure RNG for every packet,
  transmit it in the clear, and initialize a fresh cipher instance for that packet.
  Do not truncate it or derive it solely from RTP sequence/timestamp/SSRC.
- Random nonces are appropriate because collisions in this 192-bit space are
  negligible with a sound RNG. XChaCha20 itself is **not nonce-misuse-resistant**;
  an actual repeated key/nonce still repeats the keystream. Never replace the
  secure RNG with a small counter, timestamp, or weak seeded PRNG.
- Retain a static PSK supplied through a private key file. Derive independent
  directional keys locally with HKDF-SHA256 and fixed versioned direction labels;
  this requires no exchanged randomness, session state, extra packet bytes, or
  handshake. Both peers use the same agreed derivation and packet cipher counter
  convention. Record those exact conventions before implementing.
- RTP sequence, timestamps and SSRC serve the outer framing only. Their rollover,
  packet loss/reordering, and either process restarting must not affect packet
  decryptability. There is no implicit crypto rollover counter or replay bitmap.
- XChaCha20 preserves plaintext length. Crypto overhead is 24 bytes; total outer
  IPv4 overhead is **64 bytes** (20 IP + 8 UDP + 12 RTP + 24 nonce), versus v1's
  56 bytes. Keep inner MTU 1200 initially: outer IPv4 length is at most **1264**.
  A 1000-byte inner UDP application payload becomes **1092** outer IPv4 bytes.
- Raw stream encryption has no tag or authenticated associated data. Do not claim
  that a successful XOR/decryption establishes the sender's identity or that
  modified/replayed packets will be cryptographically rejected. Keep ordinary
  packet-length/IP parsing checks and bounded buffers for runtime correctness.

Reference: [libsodium raw XChaCha20 documentation](https://doc.libsodium.org/advanced/stream_ciphers/xchacha20)
explicitly specifies the 192-bit nonce, suitability of random per-message nonces,
and absence of an authentication tag. Verify the chosen Rust implementation's
raw XChaCha20 API and interoperability vectors; use a maintained implementation
rather than inventing a cipher or modifying the AEAD internals.

### Recorded v2 conventions (X0)

- Explicit local modes `client-x` / `server-x`; v1 `client` / `server` remain
  unchanged. Nothing on the wire identifies or negotiates the mode.
- Directional keys: HKDF-SHA256 with no salt (RFC 5869 zero salt), the 32-byte
  PSK as input key material, info `rtp-l3-poc/xchacha20/v2/client-to-server` or
  `rtp-l3-poc/xchacha20/v2/server-to-client`, 32-byte output. Both peers derive
  them locally at startup; nothing is exchanged.
- RTP header (12 bytes): `0x80`, marker 0 / payload type 96, 16-bit sequence,
  32-bit timestamp, 32-bit SSRC. Per process and direction, SSRC, the initial
  sequence and a timestamp offset are random (RFC 3550 section 5.1 practice);
  the sequence increments per sent packet and the timestamp is the offset plus
  monotonic elapsed 90 kHz ticks, both wrapping freely. Framing only.
- Nonce: 24 bytes from the OS CSPRNG (`rand::rngs::OsRng`, i.e. `getrandom`)
  per packet, sent in clear immediately after the RTP header.
- Cipher: raw XChaCha20 from the RustCrypto `chacha20` 0.9 crate (already in the
  lockfile), one fresh instance per packet, block counter starting at **0** — the
  libsodium `crypto_stream_xchacha20_xor` convention (HChaCha20 subkey from the
  first 16 nonce bytes; ChaCha20-IETF nonce `0^4 || nonce[16..24]`). Ciphertext
  length equals inner IPv4 length.
- Receiver is stateless: accepts byte 0 `0x80`, byte 1 exactly 96, total UDP
  payload 56–1236 bytes; ignores sequence/timestamp/SSRC; XORs; requires a
  well-formed IPv4 header whose total length equals the decrypted length.
  Anything else increments a `rejected` counter. Replays/modified packets that
  still parse are accepted by design.
- Return routing: `server-x KEY IFACE CIDR LISTEN PEER_IPV4:PORT`. The server
  only accepts from, and only sends to, the configured peer tuple. The client
  launcher binds a fixed local UDP port. If client-side NAT rewrites the port,
  the operator learns the external tuple privately and restarts the server.

### Implementation and continuation plan

- [x] X0: after the user explicitly starts this work, read this handoff and v1
      results, verify the branch/working tree, and preserve `4e1ab87` as the v1
      reference. Record the concrete v2 wire/key/counter conventions and commit
      that checkpoint before deployment.
- [x] X1: add an explicit local v2 mode or separate experimental binary, keeping
      v1 available for a controlled comparison. Add the raw cipher dependency as
      needed. Remove setup, tag and replay assumptions from the v2 packet path;
      do not auto-negotiate modes over the network. Update launcher/runtime
      readiness and statistics so v2 readiness does not wait for a handshake.
- [x] X2: make single-client return routing independent of authentication:
      configure the server's client UDP endpoint out of band and support a fixed
      client bind port. If NAT changes the tuple, observe/configure it privately
      through the authorized operator channel. Do not add a discovery handshake.
      Keep the source-restricted temporary firewalld setup and current namespace
      arrangement. No hostnames, tuples, keys or private paths belong in Git.
- [x] X3: validate a known raw XChaCha20 vector/reference and independent packet
      round trips in both directions. Exercise sequence rollover, missing/out-of-
      order packets, fresh nonces after restart, first-packet delivery without
      setup, truncation/MTU handling and cleanup. Verify v2 has no tag or control
      packets on the wire. Replays are deliberately accepted; tampering has no
      guaranteed cryptographic detection. Do not retain contrary v1 test assertions.
- [x] X4: run isolated namespace integration, then a bounded cross-host test only
      when this experiment is started. Capture from before process startup and
      prove that the first outbound packet is RTP-framed encrypted data. Verify
      forwarding/NAT to the controlled endpoint as before; keep captures private.
- [x] X5: repeat the bounded TCP/UDP, idle/loaded RTT, CPU/RSS and packet-shape
      measurements. Report setup/first-packet failures, receiver loss and ping
      delivery alongside RTT percentiles. Compare to v1 with matched settings and
      adjacent measurement windows where feasible; do not claim a speedup from
      old unmatched samples. Record the extra 8 bytes per packet and achieved
      socket buffer sizes. Keep buffer/batching optimization a separate change
      so its effect is distinguishable from the crypto/protocol change.
- [x] X6: remove every task-owned runtime firewall rule, process, interface,
      namespace and key/staging file; independently verify cleanup. Commit a
      sanitized report and final handoff, retaining key-free raw evidence privately.

Commit coherent X checkpoints and update Current state throughout, as for v1.
Do not transfer evidence, compile, or run other throughput tests during any
measurement case. Preserve failed attempts with endpoint-free summaries; Python
timeout exception strings can contain full command arguments and must not be
published. A framing/crypto change alone does not establish valid media camouflage
or expensive classification; continue to label the result as RTP-framed IP.

## Current state

**V2 complete (X0–X6).** Report: `docs/rtp-l3-poc-v2-results.md`; endpoint-free
aggregate: `experiments/rtp-l3/results/2026-10-24-v2-summary.json`. V1 results
remain in `docs/rtp-l3-poc-results.md`.

Summary: v2 delivered the first packet with no setup, and its captures held
only RTP-framed datagrams (0 non-RTP, 0 `R3P1`; v1 shows 4 control packets).
In adjacent A-B-A windows (v2, v1, v2; 09:56–10:18 UTC) TCP goodput, UDP
ladders, RTT and drop counters overlapped v1 within run-to-run variation. The
four-stream tunnel CPU was slightly lower in both v2 runs, which is not
established. V2 costs +8 bytes per packet (1264 vs 1256 max IPv4). New
observable: `server-x` forwards TUN-originated packets to the configured peer
before the client speaks. Out-of-band return-tuple configuration is the main
operational cost behind client NAT.

X5 notes: `capture-shape.py` now takes an optional max IPv4 length (1264 for
v2). A direct baseline was not repeated. Socket buffers were still 425,984
bytes, and receive overflow is still the main local drop source. Buffer or batching
changes remain a separate experiment.

**Cleanup complete and verified (X6):** all task runtime firewalld rules and
interface assignments were removed. Task processes, TUN, veth and egress
namespace are gone. The zone/policy listings, IP forwarding and link list
exactly match the pre-task snapshots. Permanent firewalld config was never
touched. The host's pre-existing iperf3 service was untouched. The server and
client task directories (source, vendor, binaries, PSKs, logs, captures) were
deleted, and local build staging was deleted. Only key-free raw evidence is
retained privately outside the checkout. No outstanding remote cleanup
obligations remain.

## Progress log

- 2026-10-24: created branch from main; verified Linux prerequisites on both hosts.
  User requires temporary firewalld openings on the public server and cleanup.
- 2026-10-24: P1 inherited-socket/TUN echo passed; proof rule and files removed.
- 2026-10-24: P2 runtime/library/launcher implemented; seven protocol tests passed.
- 2026-10-24: P3 Linux offline build/tests and disposable namespace fault integration passed.
- 2026-10-24: P4 cross-host TUN, MTU ping and controlled forwarding/NAT passed; bounded deployment active.
- 2026-10-24: P5 tools saved during active deployment; direct baseline initial connection timeout under diagnosis/retry.
- 2026-10-24: direct baseline partial after bounded failures; tunnel matrix progressing; packet-shape parser independently validated on synthetic rollover/reorder/duplicate fixture.
- 2026-10-24: primary tunnel matrix completed; down5M repeat completed; later overlap audit marks it potentially contaminated; final direct down5M check active.
- 2026-10-24: P6 explicit runtime firewall/network/process cleanup verified; remote staging and all PSKs removed.
- 2026-10-24: P5 sanitized report/aggregate completed; independent numeric/source/privacy and cleanup reviews passed; private build staging removed.
- 2026-10-24: documented the user's next experiment: raw XChaCha20 with fresh
  24-byte random per-packet nonces, static PSK, no handshake/Poly1305/replay window.
  Planning only; user explicitly deferred implementation/testing to a later agent.
- 2026-10-24: user started v2. X0 conventions recorded; remote baselines inspected read-only.
- 2026-10-24: X1–X3 v2 modes, unit tests and namespace integration passed (macOS+Linux); v1 regression integration passed.
- 2026-10-24: X4 cross-host v2 passed: no-setup first packet, MTU/forwarded ping, NAT, capture shows only RTP-framed data.
- 2026-10-24: X5 adjacent v2/v1/v2 matrix completed with no failed cases; X6 cleanup
  verified against pre-task snapshots; sanitized v2 report and aggregate committed.
