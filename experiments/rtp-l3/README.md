# Experimental RTP-framed IPv4 tunnel

Linux-only TUN runtime; portable protocol tests. This is encrypted IP in an RTP
header, **not codec media, SRTP, or demonstrated censorship resistance**. No
production integration.

The binary currently runs v3 as described below. The v4 library checkpoint Z1
adds `control` (padded authenticated PING/STATE and per-port key derivation) and
`state` (portable client/server lifecycle with caller-supplied monotonic time).
Socket integration, v4 CLI/counters and Linux validation are Z2 onward; see the
handoff's Current state for timer, replay and runtime integration conventions.

Every datagram is `RTP header (12) | random (16) | XChaCha20(inner IPv4)`, with
no handshake or setup packets. The 24-byte XChaCha20 nonce is RTP header bytes
2..10 (sequence, timestamp, high SSRC half) followed by the 16 random bytes
from the OS RNG. Each packet uses a fresh cipher (block counter 0, the libsodium
`crypto_stream_xchacha20_xor` convention). Directional keys come from the PSK
via HKDF-SHA256. IPv4 wire size is inner length + 56 bytes (1200-byte inner
packet -> 1256). There is **no tag, replay protection, sender authenticity or
forward secrecy** by design. Modified or replayed packets that still parse as
IPv4 are forwarded; header tampering garbles the decrypted packet.

Build/test with `cargo test --locked` and `cargo build --release --locked` in this
directory. The crate is deliberately outside the production Cargo workspace.

Generate an exactly 32-byte random key in a private file outside the checkout,
mode 0600, and transfer it securely. The runtime refuses group/world-readable
keys. Never print or commit keys. Both sides use the same file contents.

Server (authorized Linux account with TUN privileges):

```sh
rtp-l3-poc server KEY_FILE TUN_NAME INNER_SERVER_CIDR LISTEN_ADDRESS:PORT CLIENT_IPV4:PORT
```

Client (ordinary account supporting unprivileged user/network namespaces):

```sh
python3 scripts/client-netns.py BINARY KEY_FILE SERVER_ADDRESS:PORT TUN_NAME INNER_CLIENT_CIDR RUN_SCRIPT [CLIENT_BIND_PORT]
```

The server only accepts from, and sends to, the configured client tuple, as
seen after any client-side NAT. Learn it privately, for example by sending one
datagram from the fixed client bind port and reading it from a server capture.
Keep a server bound (with a placeholder peer while learning the tuple, then
restart it with the real one) so client datagrams never elicit ICMP. Some NATs and
paths drop return traffic to particular fixed ports; check the chosen port with
a plain UDP echo before use. The server forwards anything its TUN emits to the
configured peer, even before the client sends anything.

The launcher creates and connects UDP **before** entering `unshare -Urn`. The
socket retains host networking while the TUN and `RUN_SCRIPT` applications live
in an isolated network namespace. `RUN_SCRIPT` can configure a default route via
the TUN if server forwarding is configured. IP addresses, endpoints, firewall
rules and private deployment locations are operator-supplied, never hardcoded.
Server does not alter firewall, forwarding sysctls or NAT automatically. Restrict
its temporary UDP firewall opening to the test source. Use runtime rules only and
explicitly remove them afterwards. Closing TUN FDs removes task interfaces.

The event loop polls nonblocking TUN/UDP with bounded 64-packet batches, no
unbounded user queue, a requested 4 MiB kernel socket buffer, and 1200-byte inner
MTU. It emits JSON counters every five seconds and on SIGTERM/SIGINT. Socket
send/TUN write failures are counted and dropped. `rejected` counts foreign
sources and datagrams that are malformed or not IPv4 after decryption; iperf's
receiver report supplies final UDP loss/jitter. The Linux SO_RXQ_OVFL counter
records cumulative local receive-buffer drops (observed on the next received
packet). Counters include UDP payload bytes, excluding outer IP/UDP overhead.

See `docs/rtp-l3-poc-handoff.md` at repository root for the protocol decisions,
checkpoints and cleanup evidence, and `docs/rtp-l3-poc-v3-results.md` for the
final measurements. There is no reconnect timer, congestion controller, pacing,
FEC or fragmentation. Endpoint TCP retains its own congestion control. Use
bounded paced UDP experiments only.

For isolated Linux integration, run `sh scripts/local-integration.sh CRATE_DIR
PRIVATE_KEY_FILE OUTPUT_DIR` with absolute paths. It creates disposable nested
namespaces and checks first-packet delivery without setup, 100 MiB each way
across RTP rollover and restarts of both peers. Through `fault-proxy.py` it also
injects bounded periodic loss, duplicates, reordering (held at most 20 ms) and
ciphertext corruption. The script bounds waits and validates receiver JSON.

`benchmark.py` runs sequential TCP upload/download with one/four streams and three
20-second repetitions, followed by a paced 1000-byte UDP ladder (ten seconds per
rate, stop above 2% receiver loss). It records 100 idle ping probes, loaded ping,
receiver throughput/loss/jitter, iperf CPU, and optional client tunnel CPU/RSS via
`POC_TUN_PID`. One bounded connection retry is preserved; a second failure stops
the matrix. Supply target/port/output arguments and keep raw output private.
`sample-process.py` independently samples a task-owned server PID. The scripts
measure process CPU as percent of one core. They do not establish one-way delay.

`capture-shape.py PRIVATE_PCAP SERVER_IPV4 [MAX_IPV4_LEN]` emits endpoint-free
IPv4 packet sizes, arrival gaps, burst lengths, RTP sequence observations,
timestamp deltas and non-RTP UDP counts from an Ethernet pcap. Capture at least
the 12-byte RTP header; retain original packet lengths. Publish its aggregates
with the exact capture/case window and tcpdump's capture-drop counts. Passive
sequence gaps are not final receiver/path loss. On hosts whose tcpdump AppArmor
profile restricts output paths, write captures with `tcpdump -w - > FILE`.
