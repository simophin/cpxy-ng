# RTP/UDP L3 tunnel POC handoff

## Objective and scope

Build and measure an experimental IPv4 packet tunnel carried in RTP-framed UDP.
The immediate questions are throughput, loss, latency and observable packet shape
on the authorized test path. This does not establish resistance to censorship.
No desktop/mobile integration, DNS splitting, production rollout, or changes to
the existing proxy are requested. Keep the experiment in a standalone Rust crate.

Branch: `poc/rtp-l3-tunnel`, created from local `main` / `origin/main` at
`e112599400ec1b9c9705a136f5e41b458dfc104e`. The first commit is this handoff.

**Final protocol: v3** (handshake-free raw XChaCha20, RTP header prefix + 16
random nonce bytes; see "Final protocol" below). It is the only protocol in the
code. V1 (PSK handshake + ChaCha20-Poly1305) was removed in Y6; its last
version is at `f980c51` (v1 measurements at `4e1ab87`). V2 (24 random nonce
bytes) was superseded by v3 and lives only in history. The v1 and v2 sections
below are kept as the decision record.

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

## Original implementation (v1, completed; removed from code in Y6)

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

## Wire and session specification (v1, removed)

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

## Handshake-free XChaCha20 (v2, COMPLETE; superseded by v3)

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

## Final protocol: v3, 16 random nonce bytes (COMPLETE)

The user chose v3 as the **final decision**: after a matched comparison it
replaces v1, and the v1 handshake/AEAD code is removed. V2 remains documented
above as the 24-byte variant; v3 differs only in nonce construction and labels.

### v3 conventions (Y0)

- Packet: `RTP header (12) | random (16) | XChaCha20(inner IPv4)`. IPv4 overhead
  is **56 bytes** (20 + 8 + 12 + 16), the same as v1. A 1200-byte inner packet
  is 1256 bytes; a 1000-byte inner UDP payload is 1084 bytes.
- 24-byte XChaCha20 nonce = RTP header bytes 2..10 (16-bit sequence, 32-bit
  timestamp, first two SSRC bytes) followed by the 16 random bytes. The random
  bytes come from the OS CSPRNG for every packet. The receiver rebuilds the
  nonce from the received header, so it stays stateless. Rollover, loss,
  reordering and restarts never affect decryption.
- Uniqueness rests on the 128 random bits alone; the header prefix is a bonus.
  Probability of any repeat among n packets under one key is about n^2/2^129:
  ~1.5e-16 for a year at 10k packets/s and ~1e-10 at 2^48 packets. A repeat
  would expose only that pair's XOR of plaintexts, never the key.
- Keys: HKDF-SHA256 as v2 but with info `rtp-l3-poc/xchacha20/v3/client-to-server`
  and `.../v3/server-to-client`. Block counter 0, fresh cipher per packet.
- Receiver accepts byte 0 `0x80`, byte 1 exactly 96, UDP payload 48–1228 bytes,
  and a well-formed IPv4 packet after XOR. Header tampering now garbles the
  decrypted packet (no authentication either way).
- Server return routing is configured out of band, as in v2.

### v3 plan

- [x] Y0: record conventions (this section).
- [x] Y1: implement v3 in place of the 24-byte v2 format, with unit vectors.
- [x] Y2: Linux build/tests/clippy and namespace integration.
- [x] Y3: cross-host validation and first-packet capture.
- [x] Y4: adjacent v3 / v1 / v3 bounded matrix as in X5.
- [x] Y5: verified remote cleanup; sanitized report.
- [x] Y6: remove v1 (handshake, AEAD, replay state, control-message tooling);
      v3 becomes plain `client` / `server`; tests and integration pass.

## Next: control channel, port rotation and keepalive (v4, PLANNED)

Planning only; nothing below is implemented. It builds on v3 without changing
data packets. Defaults marked "(tunable)" are starting points for Z4
measurement, not decisions.

### Goals

- **Port rotation.** The server always listens on a fixed base port. From time
  to time it opens an extra port and announces it to every active client.
  Clients then send to the new port. No single server port carries a
  long-lived flow, much as RTP endpoints move between ports after ICE
  or hole punching.
- **Control messages.** Add an authenticated control channel inside the
  existing RTP framing. It carries registration, keepalive, announcements and
  state resynchronisation. Control packets must look like data packets on the
  wire.
- **Keepalive.** Keep the client's NAT mapping alive and detect dead paths over
  UDP. Recover by falling back to the base port.
- **Multiple clients.** The server keeps one session per client, learns each
  client's address from authenticated control, and routes TUN packets by inner
  destination address. This removes the out-of-band `PEER` argument and the
  tuple-learning procedure.

### Wire format

Every datagram stays `RTP header (12) | random (16) | XChaCha20(body)`, using
v3 nonce rules and the same directional data keys. The receiver tells data and
control apart by the first decrypted byte:

- High nibble `4`: an IPv4 data packet, handled exactly as in v3. It carries no
  tag and has no replay protection.
- `0x01`–`0x0f`: a control message. Its 16-byte tag must verify, or the
  packet is dropped and counted under `control_rejected`.
- Anything else: rejected, as in v3.

Control body: `type (1) | version (1) | length (2) | fields | zero padding`,
followed on the wire by `tag (16)`. Encrypt-then-MAC:
`tag = HMAC-SHA256(control_key, RTP header || random || encrypted body)[..16]`.
On the base port, `control_key` comes from the PSK via HKDF with info
`rtp-l3-poc/xchacha20/v4/control-client-to-server` or `.../control-server-to-client`,
and data keys stay at the v3 labels. Rotation ports use per-port keys (see
"Per-port keys" below). This re-adds the `hmac` dependency, for control only.
Control traffic is rare, so the cost is negligible.

Padding: each control packet is padded to a random IPv4 size between its
unpadded size and 260 bytes (tunable). That overlaps small data packets such as
TCP ACKs (108 B on the wire in v3). No control packet has a fixed size.

Messages (all fields big-endian):

| Type | Direction | Fields |
|------|-----------|--------|
| `0x01 PING` | client → server | session id (8), inner IPv4 (4), ping seq (4), known server instance (8), known epoch (4) |
| `0x02 STATE` | server → client | session id (8), echoed ping seq (4, 0 if unsolicited), server instance (8), epoch (4), current port (2), current port secret (32), base port (2), grace seconds (2) |

PING is both registration and keepalive. Every PING carries the client's full
state and every STATE the server's, so any single exchange resynchronises both
sides. No message needs a reliable-delivery layer of its own. An announcement
is an unsolicited STATE with a higher epoch. The client acknowledges it by
sending a PING to the new port.

### Per-port keys

- **Port secret.** When the server opens a rotation port it draws a fresh
  32-byte secret for that port from the OS RNG. The secret lives only in server
  memory and in STATE messages.
- **Key derivation.** For a rotation port, all four directional keys (data and
  control, each direction) come from HKDF-SHA256 with the port secret as input
  and a PSK-derived salt (info `rtp-l3-poc/xchacha20/v4/port-salt`). The
  labels are `rtp-l3-poc/xchacha20/v4/port/{data,control}-{client-to-server,server-to-client}`.
  The salt ties the port keys to the PSK. The base port always uses the
  PSK-derived keys, so a client can always bootstrap or fall back.
- **Delivery.** STATE carries the current port and its secret. It travels on
  the client's current path, so it is protected by that path's keys, which
  chain back to the PSK. Every STATE carries the secret, including PING
  replies, so a client that missed an announcement still gets the secret from
  its next keepalive.
- **Key selection.** The port picks the key, so the receiver never tries
  several keys and the packet carries no key hint. The server uses the key of
  the socket a packet arrived on. The client uses the key of the server source
  port. During grace the old port keeps its old secret; the secret is erased
  when the port closes.
- **Shared, not per client.** One shared PSK stays the model (Z0). Every client
  on a port gets that port's secret, so port keys separate ports and limit how
  long one key is used. They do not separate clients.
- **Server restart.** Secrets are lost on restart. A client on a rotation port
  gets no answer, falls back to the base port and receives the new instance's
  port and secret.
- **No forward secrecy.** Someone who holds the PSK and recorded the STATE
  messages can recover every port secret. Exchanging ephemeral X25519 keys in
  PING/STATE would add forward secrecy; that is a possible follow-up, not part
  of v4.

### Session and state model

- **Client session.** At start the client picks a random 64-bit session id. Its
  inner IPv4 comes from its CIDR argument. Ping seq increases strictly for the
  life of the process.
- **Server instance.** At start the server picks a random 64-bit instance id.
  Epoch starts at 0 and increases by one per rotation. A changed instance id
  tells the client that the server restarted.
- **Server session table.** Keyed by session id, each entry holds the inner
  IPv4, the path (receiving socket plus client source tuple), the last ping seq,
  the last-seen time and the last epoch the client acknowledged.
  - The server creates or updates an entry only from a PING whose tag verifies
    and whose seq is above the stored one.
  - The path moves to wherever that PING arrived, which handles NAT rebinding
    and client roaming.
  - Data packets never create or move a session.
- **Inner addresses.** A claimed inner IPv4 must lie inside the server CIDR and
  must not be the server's own address. If another live session already holds
  that address, the newer PING wins and the older session is dropped and
  counted.
- **Upstream data.** Accepted only from a source tuple that belongs to a live
  session on the socket where it arrived. Otherwise it is dropped and counted
  under `rejected`, as in v3.
- **Downstream data.** A TUN packet's destination address selects the session.
  The server sends from that session's socket to its tuple. Packets with an
  unknown destination are dropped and counted.
- **Session expiry.** A session expires after 90 s (tunable) with no valid
  packet from its tuple.
- **Shared PSK.** All clients share one PSK for the POC, so any key holder can
  impersonate another session. This is accepted (Z0 decision 1).

Replay rules:

- **Server.** It ignores a PING whose seq is not above the stored seq, so
  replayed old PINGs cannot move a session.
- **Client, same instance.** It accepts a STATE with the same instance id only
  if the epoch is at least its current epoch.
- **Client, new instance.** It accepts a different instance id only in a STATE
  that echoes an outstanding ping seq. A replayed old-instance STATE therefore
  cannot redirect the client.
- **Residual risk.** An on-path attacker can still race a fresh PING from
  another address. It could already drop traffic, so this is accepted for the
  POC.

### Port rotation

- **Server configuration:** a base port, a rotation range (`LOW-HIGH`), a
  rotation interval (mean 300 s ±50% jitter, tunable) and a grace period
  (60 s, tunable; at least the longest keepalive interval plus the full PING
  retry time).
- **Bootstrap.** The base port is always open and never carries steady
  traffic. The first STATE a client receives already names the current
  rotation port, so after one round trip the base port is used only for
  bootstrap and fallback.
- **Rotation.** The server binds a random free port in the range, retrying
  ports that are in use. It increments the epoch and sends an unsolicited STATE
  to every session over that session's current path. It does not send from the
  new port, because the client's NAT may filter unsolicited traffic from it.
- **Announcement retries.** The STATE is resent after 1, 2 and 4 s until the
  session sends a PING with the new epoch on the new port. Sessions that never
  move stay on the old port until grace ends, then fall back to the base port
  through keepalive failure.
- **Socket limit.** The server keeps at most the base port plus the current
  and previous rotation ports. A port closes when its grace ends or its last
  session moves, whichever comes later. When rotations outpace grace, the
  oldest port closes early.
- **Client switch.** On a valid announcement the client sends a PING to the new
  port immediately and moves upstream data there in the same flight.
  - Downstream from the server's address is accepted on any port it currently
    knows: base, current and the previous one during grace.
  - The client keeps one local UDP socket, so its internal tuple is unchanged.
    The NAT may still assign a new external port per destination, which the
    server learns from that PING.
- **Fresh RTP stream per port.** Each side starts a new `Tx` (new SSRC, sequence
  and timestamp offset) per server port, so each port looks like its own
  stream rather than one stream moving between ports.
- **Client socket.** The launcher's socket is created and connected outside the
  namespace. The client reads the server address with `getpeername` and then
  dissolves the connection with `connect(AF_UNSPEC)` so it can `sendto` several
  ports. The CLI does not change. Z2 must verify that an inherited socket
  behaves this way inside the namespace.

### Keepalive and liveness

- **Client PING triggers.** The client sends a PING when nothing has been sent
  *or* nothing has been received for the keepalive interval I. I defaults to
  15 s with ±20% jitter and is tunable (see Z4). Under steady bidirectional
  traffic no keepalives are sent.
- **Retry and fallback.** Every PING expects a STATE. With none after 2 s the
  client retries, up to 3 attempts in all. If all fail, the path is dead and the
  client falls back to the base port, retrying with backoff of 1, 2, 4 … 30 s.
  Data keeps flowing during retries.
- **Server.** The server never sends keepalives on its own. Only client-sent
  traffic reliably refreshes the client's NAT mapping. The server's job is to
  answer PINGs and expire idle sessions.
- **Startup.** The client sends a PING before its first data packet, without
  waiting for the reply. The first data packet can be lost if the network
  reorders it ahead of the PING. This replaces v3's "first packet with no
  setup" property with "one control packet leads the first flight".
- **Wire visibility.** Keepalives are small packets in a stream that is
  otherwise idle. Real RTP media streams rarely go fully silent, so idle
  cadence remains distinguishable. Cover traffic is out of scope for now.

### Runtime changes

- **`lib.rs`.** Add the control codec (encode/decode, tag, padding) and
  sans-I/O client and server state machines that take time as a parameter. The
  rotation, retry, expiry and replay rules can then be unit-tested on macOS
  without sockets.
- **`main.rs` (server).** Poll several UDP sockets plus the TUN, drive timers
  from the poll timeout, and keep the session table. The CLI becomes
  `server KEY IFACE CIDR LISTEN_BASE ROTATE_LOW-ROTATE_HIGH [ROTATE_SECS]`, and
  `PEER` is removed.
- **`main.rs` (client).** Use the unconnected socket, PING/STATE handling,
  switching and fallback.
- **Counters.** Add `sessions`, `rotations`, `announces_sent`,
  `announce_retries`, `switches`, `pings_sent`, `pings_unanswered`,
  `fallbacks`, `control_rejected` and `unknown_destination`. The JSON
  `protocol` becomes `xchacha20-v4`.
- **Scripts.**
  - `local-integration.sh`: runs two clients and adds NAT and rotation cases.
  - `fault-proxy.py`: gains a mode that drops STATE and PING packets. The
    proxy cannot decrypt, so it identifies them by size, or by a test-only
    hook.
  - `capture-shape.py`: reports per-port flows and inter-packet gaps while the
    flow is idle.

### Plan

- [x] Z0: decisions recorded below; per-port keys added.
- [x] Z1: control codec, per-port key derivation and state machines in
  `lib.rs`. Unit tests:
  - control round trip, and rejection of any flipped bit in the header, random
    bytes, body or tag;
  - per-port keys differ by port and direction from each other and from the base
    keys; a packet sealed for one port does not open with another port's keys;
  - data and control told apart by the first byte, and padding lengths within
    range;
  - session create/move only from a fresh PING; replayed PINGs and STATEs
    ignored; instance-change acceptance rules;
  - rotation with announcement retries; grace close; socket limit;
  - keepalive triggers, retry and fallback timing; session expiry; inner-address
    conflicts.
- [ ] Z2: server and client runtimes, CLI and counters. On Linux: build, tests,
  clippy with warnings denied, and the existing data-path integration still
  passing.
- [ ] Z3: namespace integration with two concurrent clients behind a
  masquerading router namespace. Rotation runs every 10 s during 100 MiB
  transfers each way. Required outcomes:
  - every client passes through at least 3 ports;
  - no transfer stalls for more than 5 s;
  - dropped announcements are recovered by retries or keepalive;
  - flushing the router's conntrack (a NAT rebinding) is relearned from the
    next PING;
  - blocking the current rotation port ends in base-port fallback;
  - a server restart is detected through the instance id, and sessions
    re-register;
  - each rotation port uses its own keys, and a client that missed an
    announcement picks up the secret from its next keepalive.
- [ ] Z4: cross-host validation. Firewalld runtime rules open the base port and
  rotation range for the client's /32 source only.
  1. Measure the client NAT's UDP mapping timeout and per-destination port
     behaviour with idle probes at 30, 60, 120 and 180 s and with several
     server ports. Choose I from that.
  2. Run rotation every 30 s for 10 minutes under continuous paced UDP and
     ping, then leave the tunnel idle for 10 minutes and confirm keepalives
     hold the mapping.
- [ ] Z5: bounded comparison of rotation (20 s) against none. Measure TCP
  goodput, plus 5 Mbit/s UDP loss and gaps around each switch. Run packet-shape
  captures, including what the base port sees.
- [ ] Z6: verified cleanup of runtime rules, processes, interfaces, staging
  and keys; sanitized report.

### Decisions (Z0, 2026-10-24)

1. **Keys.** One shared PSK with session ids; no per-client keys, so no trial
   decryption and no key hint. Added: each rotation port gets an ephemeral
   secret delivered in STATE (see "Per-port keys").
2. **Rotation cadence.** Mean 300 s ±50%. Shorter intervals spread load across
   more ports but add more switch events and NAT mappings.
3. **Client local port.** Keep one local socket. Rotating it as well would need
   more sockets created outside the namespace; it stays a possible follow-up,
   since it would also avoid the observed fixed-port return problem.
4. **Cover traffic.** None; idle keepalive cadence is accepted as observable.

## Current state

**Z1 complete; next: Z2 runtime integration.** `lib.rs` exposes `control` and
`state` modules for the authenticated codec, per-port keys and sans-I/O client /
server lifecycle. All 17 portable tests pass on macOS, including the six v3
regressions. `cargo clippy --locked --all-targets -- -D warnings` passes.
The binary, CLI, counters and Linux integration script still run v3; v4 Linux
and cross-host validation remain Z2–Z4. No remote resources were created and
there are no cleanup obligations.

Z1 integration conventions / clarified edge cases:

- Time is monotonic whole seconds; call timer methods at least once per second.
  The runtime samples jitter with `rotation_delay` / `keepalive_delay`, sets the
  client's interval for each new idle window, binds candidate rotation ports,
  and passes a fresh OS-random secret to `Server::rotate` only after bind succeeds.
  Failed binds must leave epoch/session state unchanged. Runtime owns fresh Tx
  streams per port, socket closing, counters and selecting keys by local/server
  port. Choose a bounded session-table capacity when constructing the server.
- Control version is 4. The body length includes the 4-byte type/version/length
  header and fields, excludes zero padding and the 16-byte tag: PING = 32 bytes,
  STATE = 66 bytes. Their minimum outer IPv4 sizes are 104 and 138 bytes.
  The maximum padded outer IPv4 size is 260 bytes.
- Every retry gets a fresh increasing PING sequence, since duplicate PINGs are
  deliberately ignored by the server. Only the latest sequence is outstanding.
  A new-instance STATE must echo it, and acceptance consumes that echo.
  Equal-epoch unsolicited/replayed STATE is a no-op; it cannot change the port
  or secret, reset retry timers, or prolong previous-port grace.
- After three unanswered attempts at 2-second spacing, fallback selects the
  base port. Base probes then use 1, 2, 4, 8, 16, 30, 30... second spacing,
  until a valid STATE restores the current rotation path.
- Announcement retry deadlines are +1, +2 and +4 seconds from announcement.
  A PING acknowledges rotation only on the new socket with the current instance
  and epoch. Retire the previous socket after **both** grace has elapsed and its
  last session has left (the later condition, per the socket-limit rule above).
  A subsequent rotation evicts the oldest socket and its stranded sessions to
  maintain the three-socket limit. These clients recover through base fallback.
- Pass only authenticated, direction-checked PING/STATE to the state machines.
  Check the server source IP before client handling; refresh data liveness only
  after decoding a valid IPv4 packet on a known path. Data never registers or
  moves sessions. Expired sessions forget sequence state; replay suppression is
  scoped to a live session, not a persistent global anti-replay database.
- Client `state` returns a PING when switching; `None` can also mean an accepted
  reply requiring no switch. Server replies and announcements retain the existing
  session path so the runtime seals them with that path's keys.

**V3 complete (Y0–Y6); it is the final protocol.** Report: `docs/rtp-l3-poc-v3-results.md`; aggregate: `experiments/rtp-l3/results/2026-10-24-v3-summary.json`. Cross-host validation and the adjacent v3/v1/v3 matrix (10:59–11:19 UTC) showed no measurable difference from v1 (the path drifted during the window), identical 1256/108-byte data/ACK sizes, and 0 non-RTP packets for v3. Cleanup verified against fresh pre-deployment snapshots; remote staging and PSKs deleted. Y6 removed v1: handshake/control messages, ChaCha20-Poly1305, HMAC, replay window and their dependencies; `client`/`server` now run v3 (server takes the client tuple as a 6th argument; the launcher takes an optional bind port instead of a mode). The v3 integration script replaced the v1 one, the fault proxy lost its control-drop logic, and `capture-shape.py` counts non-RTP UDP instead of `R3P1` types. Six unit tests pass on macOS and Linux, Linux release build and clippy pass, and the namespace integration passes. Client staging/key deleted again; no remote resources or cleanup obligations remain.

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
- 2026-10-24: user chose a 16-random-byte nonce (v3) as the final protocol; Y0 conventions recorded.
- 2026-10-24: Y1–Y5 v3 implemented, validated cross-host, matched against v1 (no measurable difference); cleanup verified.
- 2026-10-24: Y6 removed v1 from the code; v3 is the only protocol. Linux tests, clippy and integration pass.
- 2026-10-24: planned v4: authenticated PING/STATE control, server port rotation with announcements, keepalive/fallback, multi-client sessions (Z0–Z6). Not implemented.
- 2026-10-24: Z0 decided: shared PSK, 300 s ±50% rotation, one client local port, no cover traffic; added ephemeral per-port secrets delivered in STATE.

- 2026-10-25: Z1 codec checkpoint: authenticated padded PING/STATE, base and
  per-port directional keys, strict framing/body validation and data dispatch.
  Codec tests include every single-bit mutation and truncated packets.
- 2026-10-25: Z1 state machines complete: session registration/roaming/conflicts,
  bounded capacity, expiry, rotation/retries/grace, replay/instance checks,
  keepalive and base fallback. Seventeen macOS tests and clippy with warnings
  denied pass. No remote deployment; runtime integration is Z2.
