# Experimental RTP-framed IPv4 tunnel

Linux-only TUN runtime; portable protocol tests. This is encrypted IP in an RTP
header, **not codec media, SRTP, or demonstrated censorship resistance**. No
production integration.

The binary implements one experimental protocol: authenticated padded
PING/STATE/PROBE control, learned client sessions, server UDP port rotation with
ephemeral per-port keys, and client keepalive/fallback. The portable `control` and `state` modules implement the
codec and lifecycle; the Linux runtime owns sockets, timers and counters.

Every data datagram is `RTP header (12) | random (16) | XChaCha20(inner IPv4)`,
with no tag on data. Authenticated PING/STATE and candidate PROBE validation precede the first data flight; STATE
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
PING may move a tuple after NAT rebinding. PROBE authenticates a candidate
without moving the established data route.

The base port remains open for bootstrap/fallback. The server immediately opens
a rotation port, then rotates every 300 seconds ±50% by default. ROTATE_SECS
changes that mean (minimum 2); zero keeps the initial rotation port fixed for
matched no-rotation measurements. Registration and keepalives are unchanged.
The range must contain at least three non-base ports. Binding retries occupied ports before changing epoch or sessions; a
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

The launcher creates and connects the bootstrap UDP socket **before** entering
`unshare -Urn`. The runtime disconnects it with AF_UNSPEC, restoring the initial
local tuple if Linux clears the implicit binding. A private Unix socket broker
remains in the host namespace and supplies fresh bound, unconnected UDP sockets
via SCM_RIGHTS. It does not handle keys or tunnel payloads. The optional launcher
bind port fixes only the initial bootstrap port.

STATE advertises a candidate; authenticated PROBE/STATE validates its return path
before the client switches and sends registration PING. The server keeps routing
data on the established path until that PING. Three unanswered one-second
candidate challenges replace the candidate socket, while data and keepalives
continue on the working socket. Base recovery also renews failed sockets. The
client retains at most base/active/previous/candidate descriptors. The TUN comes
up, and RUN_SCRIPT starts, only after validated-path registration completes.
Both peers must be rebuilt together for the added PROBE control message.

`RUN_SCRIPT` can configure a default route via the TUN if server forwarding is
configured. Addresses, endpoints, firewall rules and private deployment locations
are operator-supplied. Server does not alter firewall, forwarding sysctls or NAT
automatically. Restrict temporary public UDP openings to test source /32s, use
runtime rules only, and explicitly remove them afterwards.

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
`fallbacks`, `unknown_destination` and `rotation_bind_failures` describe the session
lifecycle.

See the [protocol specification](../../docs/rtp-l3-protocol.md),
[handoff and diagnosis checklist](../../docs/rtp-l3-poc-handoff.md),
[Z4 repair acceptance](../../docs/rtp-l3-z4-repair-results.md), and
[historical cross-host results](../../docs/rtp-l3-cross-host-results.md), including
the preserved original idle failure. Earlier measured configurations are historical
evidence, not supported modes. There is no congestion controller, pacing,
FEC or fragmentation. Endpoint TCP retains its own congestion control. Use
bounded paced UDP experiments only.

For isolated Linux integration, run `sh scripts/local-integration.sh CRATE_DIR
PRIVATE_KEY_FILE OUTPUT_DIR` with absolute paths. It creates disposable nested
namespaces and checks registration, idle rotation, 100 MiB each way across RTP
rollover, inherited socket `AF_UNSPEC`/`sendto` across namespaces and restarts
of both peers. Through the per-port loopback `fault-proxy.py` it also injects
bounded periodic loss, duplicates, reordering (held at most 20 ms) and
ciphertext corruption. The script bounds waits and validates receiver JSON and
session counters.

For the Z3 matrix, run `timeout 360 unshare -Urn python3
scripts/z3-integration.py CRATE_DIR PRIVATE_KEY_FILE OUTPUT_DIR` on Linux with
absolute paths. It needs Python 3.9+, ip, iptables, nsenter, unshare and iperf3.
If administrative tools are outside the unprivileged shell PATH, prepend
`/usr/sbin:/sbin` before invoking the harness.
Two clients with the same initial bootstrap UDP port use separate outer
namespaces behind a masquerading router, then inherit sockets into their TUN namespaces. With a
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

`POC_TRACE_CONTROL=1` enables opt-in return-delivery diagnostics on both peers.
Each event records wall-clock and monotonic microseconds, a SHA-256 fingerprint
of the encrypted UDP payload, and receive/decode/PING/PROBE/STATE/send stages.
Control metadata contains only ephemeral session id, sequence/echo and epoch; no secret,
inner address or peer address is logged. Timer events expose pending-sequence
replacement and fallback. STATE outcomes distinguish accepted completion from
ignored announcements, superseded echoes and invalid state. The existing
state-machine policy and wire format are unchanged. Successful send means
kernel acceptance, not proven wire delivery. Trace all received packets to
identify decoder failures; use bounded low-rate diagnostics because hashing and
JSON logging can affect performance. Startup trace events may precede `ready`.

Run `python3 scripts/control-trace.py CLIENT_JSONL SERVER_JSONL
[SERVER_ETHERNET_PCAP]` to correlate datagrams independently of clock alignment,
join each fresh PING to its solicited STATE, and emit endpoint-free aggregates
with numbered ports/sessions. Optional full-payload Ethernet pcap fingerprints
separate server-capture observation from runtime reception. No keys are needed.
Publish capture-drop counts and run boundaries with the aggregate; missing
receive events alone cannot locate network loss or exclude logging/boundary
limitations. Relative clock-offset bounds use matched datagrams and assume a
stable offset and nonnegative transit times. They do not establish one-way delay.
`python3 scripts/test-control-trace.py` checks loss, ignored replies, successful
completion, duplication, send failures and pcap payload parsing.

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
1084-byte outer IPv4 packets produced by 1000-byte inner UDP payloads.
It reports cross-port packet gaps, all observed port transitions (including
reordering between old/new ports), and base-port packet
counts without publishing endpoint identities. Gaps of five seconds or more
are listed separately as phase boundaries **or outages**, never silently
counted as acceptable. TCP packets of the same length may also match, so use
isolated UDP capture windows or pass CASE_SUMMARY_JSON as the optional fourth
argument to restrict capture timestamps to recorded UDP case windows. This
requires synchronized server/client wall clocks; record any observed offset.
These capture observations supplement receiver loss/intervals, and do not
replace them. Always report tcpdump's capture-drop count.

The `idle` mode of `cross-host-run.py` skips the active UDP phase and performs
only startup ping, a 600-second idle window and post-idle checks. A failed
shorter diagnostic may be stopped early, but must be reported with its actual
duration and cannot be claimed as a completed ten-minute pass.

Pass CLIENT_TRACE_JSONL after CASE_SUMMARY_JSON to `switch-gaps.py` to report
capture gaps and receiver interval loss within three seconds of each runtime
path event during UDP cases. It estimates runtime origin from the run script's
`start_utc`; timers have whole-second precision and script startup adds a small
offset. Optional `capture_minus_client_clock_seconds` in the case summary
corrects measured wall-clock offset. The broad windows include adjacent
intervals and do not attribute all nearby loss to switching. Raw capture port
transitions can include many reordered packets around a single runtime switch.

Comparison helpers accept `--start-at CASE` for a bounded continuation after a
failed case and `--continue-on-error` to attempt remaining independent cases.
Failures are recorded separately and the process exits nonzero if any fail.
Keep the failed attempt, restart/window boundaries and retry count in reports;
never merge failed setup attempts into successful UDP samples.

Run `sh scripts/candidate-integration.sh CRATE_DIR PRIVATE_KEY_FILE OUTPUT_DIR`
for an isolated regression that drops candidate replies, forces a socket
replacement and checks uninterrupted UDP/ping delivery on the established path.

The runtime reports `protocol: "rtp-l3"`. There is one format and no legacy
mode or negotiation. Both peers must use the unified key labels and
reserved-zero control header documented in the specification. Historical JSON
artifacts retain their recorded identifiers and do not imply compatibility.

### Controlled destination-port probe

`python3 scripts/port-matrix.py server BIND_IP PORT1,PORT2,... --seconds 100`
starts a bounded plain UDP responder. Use source-restricted temporary runtime
firewall rules, with operator-owned cleanup. Its JSONL contains private observed
endpoints: never commit raw server output. Each request gets an immediate reply
from every bound source port to the tuple observed for that destination.

`python3 scripts/port-matrix.py client SERVER PORT1,PORT2,... --rounds 12 --window .5 --seed 25`
uses one bound socket, shuffles destination order each round, and publishes only
numbered ports/mappings and per-trial delivery/latency. Correlation tokens reject
late replies from previous trials. Run `python3 scripts/test-port-matrix.py`
for loopback delivery, sanitization and CLI-bound checks. No tunnel wire format,
socket strategy, rotation or keepalive setting changes. First-contact cross-port
replies and already-contacted ports have different filtering history. Missing
socket delivery alone cannot distinguish filtering from network loss; this
immediate plain UDP diagnostic does not measure idle timeout or complete Z4.

### Independent delayed probes

`python3 scripts/idle-matrix.py server BIND_IP PORT1,PORT2,... --seconds 240 --private-log PRIVATE_SERVER_JSONL`
starts a bounded scheduler. Each request gets exactly one reply from the requested
source port after its requested delay, with no immediate acknowledgement.
Use operator-owned source-/32-restricted runtime firewall openings and cleanup.

`python3 scripts/idle-matrix.py client SERVER PORT1,PORT2,... --delays 5,30,60,120,180 --rounds 2 --private-log PRIVATE_CLIENT_JSONL`
randomizes same-source and cross-source trials across every destination/delay.
Every trial uses a fresh silent socket and two separate fresh immediate-control
sockets, at the start and deadline. All sockets stay open until the run ends to
prevent local port reuse. No retry, control or early reply touches the silent
socket. Requests and replies are fixed 192-byte plain JSON UDP payloads (220
outer IPv4 bytes), within the tunnel control size range, without RTP/encryption.
The reply window follows the delay; choose a server lifetime covering client
startup, maximum delay, launch staggering and reply windows. Both sides are
bounded; the client permits at most 128 trials/384 sockets. Private logs are
created exclusively with mode 0600 and include correlation tokens, fingerprints
and observed endpoints. Never commit those logs. Client stdout is endpoint-free.

`python3 scripts/idle-evidence.py PRIVATE_CLIENT_JSONL PRIVATE_SERVER_JSONL --server-pcap PRIVATE_PCAP --server-capture-drops COUNT`
joins server receipt/send calls and client reception with optional exact UDP
payload fingerprints in interface captures. `--client-pcap` and
`--client-capture-drops` add client-interface evidence when available. Classic
Ethernet, raw IPv4 and Linux cooked captures are supported; use full snaplen and
record capture coverage and final drop counts. Missing capture evidence is null,
not zero. Client logs also record every socket receive before validation;
the analyzer separates accepted replies from rejected datagrams. Older logs
without this instrumentation produce null socket/rejection counts. Output
contains only numbered trials/ports/mappings. A successful
send, captured packet or matched immediate control does not prove that a silent
mapping survives, identify an exact NAT timeout or complete Z4. Matched tunnel
traces and justified remediation remain separate work.

Run `python3 scripts/test-idle-matrix.py` with UDP socket access for real delayed
delivery/isolation, negative delivery, private-file bounds, capture correlation
and sanitization fixtures.

Candidate diagnostics add `path_candidates`, `candidate_probes`, `candidate_retries`
and `socket_renewals` counters. `POC_TRACE_CONTROL` fingerprints packets within
the 260-byte control IPv4 bound; large data packets remain counted without
per-packet tracing. `connected` reports initial validated registration.

Active uploads also request a liveness PING after two seconds without input.
An unanswered PING starts a same-port candidate repair on a separate descriptor;
registration still commits the replacement tuple only after validation. Active
repair probing is bounded to eight seconds before ordinary base recovery resumes.
`active_repairs` counts these events separately from base `fallbacks`. Run
`sh scripts/repair-integration.sh CRATE_DIR PRIVATE_KEY_FILE OUTPUT_DIR` to block
a previously working tuple and verify same-port recovery with a sub-five-second
conservative receiver stall bound. The 15-second application-idle mean is unchanged.
