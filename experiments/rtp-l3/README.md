# Experimental RTP-framed IPv4 tunnel

Linux-only TUN runtime; portable protocol tests. This is encrypted IP in an RTP
header, **not codec media, SRTP, or demonstrated censorship resistance**. No
production integration.

The binary runs v4: authenticated padded PING/STATE control, learned client
sessions, server UDP port rotation with ephemeral per-port keys, and client
keepalive/fallback. The portable `control` and `state` modules implement the
codec and lifecycle; the Linux runtime owns sockets, timers and counters.

Every data datagram is `RTP header (12) | random (16) | XChaCha20(inner IPv4)`,
with no tag on data. An authenticated PING precedes the first data flight; STATE
returns the current rotation port and secret. Control uses the same framing with
a padded encrypted body and a 16-byte HMAC tag (104–260 outer IPv4 bytes for
PING, 138–260 for STATE). The 24-byte XChaCha20 nonce is RTP header bytes 2..10
(sequence, timestamp, high SSRC half) followed by the 16 random bytes from the
OS RNG. Each packet uses a fresh cipher (block counter 0, the libsodium
`crypto_stream_xchacha20_xor` convention). Directional keys come from the PSK
via HKDF-SHA256 on the base port; rotation ports derive independent data and
control keys from the PSK and a fresh port secret delivered in STATE. IPv4 wire
size is inner length + 56 bytes (1200-byte inner packet -> 1256). Data has **no
tag, replay protection or sender authenticity**, and the protocol has **no
forward secrecy**, by design. Control authenticates session registration and
changes of peer tuple; data cannot create or move a session. Modified or
replayed packets that still parse as IPv4 are forwarded; header tampering
garbles the decrypted packet.

Build/test with `cargo test --locked` and `cargo build --release --locked` in
this directory. The crate is deliberately outside the production Cargo
workspace.

Generate an exactly 32-byte random key in a private file outside the checkout,
mode 0600, and transfer it securely. The runtime refuses group/world-readable
keys. Never print or commit keys. Both sides use the same file contents.

Server (authorized Linux account with TUN privileges):

```sh
rtp-l3-poc server KEY_FILE TUN_NAME INNER_SERVER_CIDR LISTEN_ADDRESS:BASE_PORT ROTATE_LOW-ROTATE_HIGH [ROTATE_SECS]
```

Client (ordinary account supporting unprivileged user/network namespaces):

```sh
python3 scripts/client-netns.py BINARY KEY_FILE SERVER_ADDRESS:PORT TUN_NAME INNER_CLIENT_CIDR RUN_SCRIPT [CLIENT_BIND_PORT]
```

The server learns each client's source tuple and inner IPv4 from authenticated
PINGs. TUN destinations select the return session; unknown destinations are
dropped. The session table is capped at 1024 entries, idle expiry is 90 seconds,
and conflicting inner addresses replace the older session and are counted. Only
PING may move a tuple after NAT rebinding.

The base port remains open for bootstrap/fallback. The server immediately opens
a rotation port, then rotates every 300 seconds ±50% by default. ROTATE_SECS
changes that mean (minimum 2); zero keeps the initial rotation port fixed for
matched no-rotation measurements. Registration and keepalives are unchanged. The range must contain at least three non-base
ports. Binding retries occupied ports before changing epoch or sessions; a
failed rotation leaves the current path intact and retries next interval. At
most the base, current and previous rotation sockets remain installed. Previous
sockets retire after both 60-second grace and the last session leaves; a
subsequent rotation evicts the oldest port to bound resources.

The client defaults to 15 seconds ±20% keepalive jitter, with three attempts
spaced two seconds apart and base fallback with 1–30 second backoff. Set
`POC_KEEPALIVE_SECS` in the launcher environment to change the mean (5–45, so
the maximum jitter and retries fit the fixed 60-second grace). Each port uses a
fresh RTP transmit stream. Under continuous bidirectional data, idle keepalives
are suppressed. Startup data may be lost if it overtakes registration.

The launcher creates and connects UDP **before** entering `unshare -Urn`. The
runtime reads the server address, disconnects with `AF_UNSPEC` and uses `sendto`
on that same inherited socket, preserving its local port. The socket retains
host networking while the TUN and `RUN_SCRIPT` applications live in an isolated
network namespace. `RUN_SCRIPT` can configure a default route via the TUN if
server forwarding is configured. IP addresses, endpoints, firewall rules and
private deployment locations are operator-supplied, never hardcoded. Server does
not alter firewall, forwarding sysctls or NAT automatically. Restrict its
temporary base and rotation UDP firewall openings to the test source. Use
runtime rules only and explicitly remove them afterwards. Closing TUN FDs
removes task interfaces.

The event loop polls nonblocking TUN/UDP with bounded 64-packet batches, no
unbounded user queue, a requested 4 MiB kernel socket buffer, and 1200-byte
inner MTU. It emits JSON counters every five seconds and on SIGTERM/SIGINT.
Socket send/TUN write failures are counted and dropped. `rejected` counts
foreign/unknown paths and decoder failures; `control_rejected` counts malformed
decoder input and wrong-direction or rejected PINGs (encrypted malformed input
cannot always be classified as data/control); iperf's receiver report supplies
final UDP loss/jitter. The Linux SO_RXQ_OVFL counter records cumulative local
receive-buffer drops (observed on the next received packet). UDP byte counters
include control and data, excluding outer IP/UDP overhead; packet/plain-byte
counters count data only. Separate control packet counters, `sessions`,
`session_conflicts`, `rotations`, `announces_sent`, `announce_retries`,
`switches`, `pings_sent`, `pings_unanswered` (expired PING deadlines),
`fallbacks`, `unknown_destination` and `rotation_bind_failures` describe the v4
lifecycle.

See `docs/rtp-l3-poc-handoff.md` at repository root for the protocol decisions,
checkpoints and cleanup evidence, and `docs/rtp-l3-poc-v3-results.md` for the
final measurements. There is no congestion controller, pacing, FEC or
fragmentation. Endpoint TCP retains its own congestion control. Use bounded
paced UDP experiments only.

For isolated Linux integration, run `sh scripts/local-integration.sh CRATE_DIR
PRIVATE_KEY_FILE OUTPUT_DIR` with absolute paths. It creates disposable nested
namespaces and checks registration, idle rotation, 100 MiB each way across RTP
rollover, inherited socket `AF_UNSPEC`/`sendto` across namespaces and restarts
of both peers. Through the per-port loopback `fault-proxy.py` it also injects
bounded periodic loss, duplicates, reordering (held at most 20 ms) and
ciphertext corruption. The script bounds waits and validates receiver JSON and
v4 counters.

For the Z3 matrix, run `timeout 360 unshare -Urn python3
scripts/z3-integration.py CRATE_DIR PRIVATE_KEY_FILE OUTPUT_DIR` on Linux with
absolute paths. It needs Python 3.9+, ip, iptables, nsenter, unshare and iperf3.
Two clients with the same local UDP port use separate outer namespaces behind
a masquerading router, then inherit sockets into their TUN namespaces. With a
10-second rotation mean and 5-second keepalive mean it checks concurrent paced
100 MiB TCP transfers each way, at least three new ports per transfer, receiver
interval stalls below five seconds, lost announcements/retries, missed-secret
recovery by PING, conntrack flush with forced source-port rebinding, base fallback
and server restart while clients stay running. The conntrack flush uses netlink;
no conntrack executable is needed. All firewall and routing changes live inside
disposable namespaces. Raw logs stay in the private output directory; only
`summary.json` is suitable for publishing after review. The private key and
output directory remain caller-owned and must be removed after validation.

`POC_TRACE_PATHS=1` enables JSON path/rotation/registration events used by this
harness. They include port, epoch, instance-change and tuple-change indicators,
never keys or peer addresses. Session ids in registration events are ephemeral.
The explicit test hook `POC_TEST_DROP_ANNOUNCEMENTS_FILE` suppresses unsolicited
STATE and its retries while that marker file exists, emitting
`test_announcement_drop` events. Solicited STATE replies still pass. The harness
uses this only for the missed-secret case; the other loss case drops short UDP
packets with router firewall rules. Leave the hook unset for normal runs.

`benchmark.py` runs sequential TCP upload/download with one/four streams and
three 20-second repetitions, followed by a paced 1000-byte UDP ladder (ten
seconds per rate, stop above 2% receiver loss). It records 100 idle ping probes,
loaded ping, receiver throughput/loss/jitter, iperf CPU, and optional client
tunnel CPU/RSS via `POC_TUN_PID`. One bounded connection retry is preserved; a
second failure stops the matrix. Supply target/port/output arguments and keep
raw output private. `sample-process.py` independently samples a task-owned
server PID. The scripts measure process CPU as percent of one core. They do not
establish one-way delay.

`capture-shape.py PRIVATE_PCAP SERVER_IPV4 [MAX_IPV4_LEN]` emits endpoint-free
IPv4 packet sizes, arrival gaps, burst lengths, RTP sequence observations,
timestamp deltas and non-RTP UDP counts from an Ethernet pcap. Server ports are
numbered without publishing their values. Per-port gaps include idle time
across SSRC changes; per-stream rows separate port and SSRC. Only packets to or
from the supplied server are included. Capture at least
the 12-byte RTP header; retain original packet lengths. Publish its aggregates
with the exact capture/case window and tcpdump's capture-drop counts. Passive
sequence gaps are not final receiver/path loss. On hosts whose tcpdump AppArmor
profile restricts output paths, write captures with `tcpdump -w - > FILE`.

Cross-host Z4 helpers (operator supplies all endpoints and private paths):
`nat-probe.py server BIND_IP PORT,PORT,PORT` and matching
`nat-probe.py client SERVER_IP PORT,PORT,PORT` test silent 30/60/120/180-second
return reachability from several destination ports. Each deadline has its own
client socket; all destinations receive an initial request, and delayed replies
also test cross-port filtering. Raw output includes peer tuples: keep it private.
This establishes a lower bound when replies survive; it cannot identify an exact
mapping timeout or distinguish expired mappings from filtering/path loss.

Run `cross-host-run.py endurance INNER_SERVER_IP IPERF_PORT PRIVATE_OUTPUT_DIR`
inside the client namespace using a small operator-owned RUN_SCRIPT. It sends
5 Mbit/s UDP with concurrent ping for 600 seconds, then waits 600 seconds with
no application traffic before MTU ping and reverse UDP. `comparison` runs
20-second one/four-stream TCP and 60-second 5 Mbit/s UDP in each direction.
Raw receiver intervals are retained in the endpoint-free summary to compare
loss/gaps around separately recorded rotation events. Server/capture lifetime,
source-restricted runtime rules and verified cleanup remain operator-owned.

`switch-gaps.py PRIVATE_ETHERNET_PCAP SERVER_IPV4 BASE_PORT` isolates the
1084-byte outer IPv4 packets produced by 1000-byte inner UDP payloads in v4.
It reports cross-port packet gaps, all port transitions, and base-port packet
counts without publishing endpoint identities. Gaps of five seconds or more
are listed separately as phase boundaries **or outages**, never silently
counted as acceptable. TCP packets of the same length may also match, so use
isolated UDP capture windows or pass CASE_SUMMARY_JSON as the optional fourth
argument to restrict capture timestamps to recorded UDP case windows. This
requires synchronized server/client wall clocks; record any observed offset.
These capture observations supplement receiver loss/intervals, and do not
replace them. Always report tcpdump's capture-drop count.
