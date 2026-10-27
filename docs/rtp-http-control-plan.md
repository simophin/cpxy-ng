# HTTP-controlled RTP transport implementation plan

Status: implementation in progress on `feat/http-controlled-rtp`. This document does not change the running
protocol. Implement the core server and Linux client first; mobile integration
and operational deployment are deferred. The existing protocol remains described
in [rtp-l3-protocol.md](rtp-l3-protocol.md) until implementation lands.

## Progress

- [x] Inspect shared worker/mobile and generation-scoped admission APIs.
- [x] Freeze v2 encodings, replay/retry rules and state transitions in
  [rtp-http-protocol.md](rtp-http-protocol.md).
- [ ] Implement authenticated codecs and deterministic state machines/vectors.
- [ ] Integrate bounded HTTP, persistence and supervised allocation lifecycle.
- [ ] Integrate Linux transport, rotation, repair and buffering.
- [ ] Run local acceptance fixtures and record cleanup/environment limits.
- [ ] Remove superseded core paths, adapt mobile boundary, finish docs and PR.

## Objective and agreed constraints

Replace base-port UDP bootstrap and server-driven rotation with an always-plain
HTTP control endpoint on a configured TCP port. HTTP carries encrypted,
authenticated JSON-RPC requests and responses. There is no TLS and no base UDP
port, UDP fallback service, or server-wide rotation epoch.

The client obtains a dedicated random UDP port and fresh transport secret,
establishes its UDP path, and rotates that allocation every five minutes.
The server dispatches inner IPv4 traffic using one active allocation per inner
address. Two allocations may overlap during handover. Allocate/firewall work
must never block packet processing.

Peers possessing the shared PSK are trusted members. UUIDs identify persisted
client state; they are not credentials or security boundaries between members.
Any member can claim a static inner address without first allocating it by RPC.
Do not introduce per-peer certificates, permission systems, or forward secrecy.
Authentication and replay protection still protect against outside attackers.

Keep inner IPv4 MTU 1200, existing selected-network forwarding policy, RTP-shaped
framing, and scoped firewalld ownership. This is not SRTP or a claim of traffic
camouflage. Do not add FEC, reliable data delivery, fragmentation, IPv6 inner
packets, mobile UI, host deployment, or cross-host coordination in this work.

## Repository implementation map

- `rtp-l3/src/lib.rs`: replace raw XChaCha20 framing with authenticated framing,
  directional key derivation and replay windows.
- `rtp-l3/src/control.rs`: replace PING/STATE/PROBE allocation machinery with
  bounded UDP path, activation and liveness messages.
- `rtp-l3/src/state.rs`, `runtime.rs`, `worker.rs`, `worker_api.rs`: replace
  base/current/previous server rotation with per-allocation state, client-driven
  handover, a single downstream dispatcher, and injectable clock/I/O contracts.
- `rtp-service/src/{config,cli,lib,network,broker,ports}.rs`: control listener,
  stable UUID storage, address persistence, HTTP socket bypass, allocation
  lifecycle, supervisor integration and CLI configuration.
- `port-admission`: reuse generation-scoped socket/firewall leases, journaling,
  reconciliation and independent cleanup. It remains protocol independent.
- `experiments/rtp-l3`, `rtp-service/tests`: update the Linux CLI/fixtures to the
  new protocol and retain relevant packet, routing and cleanup checks.
- Audit `rtp-mobile` and its worker API consumers before changing shared APIs.
  Keep workspace builds viable through explicit interface adaptation or a
  clearly reported unsupported-protocol result. Do not silently retain the old
  insecure wire format or undertake mobile integration in this task.

Use existing abstractions where they fit. A small control module/crate is fine;
avoid making privileged firewall code depend on HTTP or RTP codecs. Inspect
current code and repository instructions before editing. Work on a dedicated
implementation branch, with reviewable commits for the phases below.

## Identity, addressing and persistent state

The client creates and atomically persists a random UUID once, outside temporary
runtime state. Store no secrets in that identifier. The server persists UUID to
inner-IPv4 mappings atomically and serializes concurrent allocation requests.

Configure a tunnel subnet, server address and dynamic pool. Exclude network,
broadcast, server and configured static reservations. Persisted dynamic mappings
survive disconnects and process restarts; UDP lease expiry does not release an
address. Do not automatically recycle an offline client's dynamic address in
this first implementation. Pool exhaustion is an explicit error; reclamation
is an administrative policy for later work.

Static clients bypass `allocate_ip`. Their address must still be a usable
address in the configured tunnel subnet. Recommend static addresses outside the
dynamic pool or explicitly reserved. Static claims within the dynamic pool must
be recorded/reserved before use so a later dynamic allocation cannot collide.
Reject an address already assigned or actively claimed by another UUID; do not
silently steal its route. A UUID cannot concurrently use two inner addresses in
the first implementation. These are configuration-consistency checks, not an
attempt to restrict malicious PSK holders. Lost/copied UUID state has explicit
diagnostics; cloning a UUID does not create an independent client identity.

Persist address mappings/reservations, not live UDP keys or replay counters.
After server restart all old allocations are invalid; clients recreate them.
Journal firewall ownership separately, with no cryptographic secrets.

## HTTP control protocol

Use JSON-RPC 2.0 inside an AEAD-protected HTTP POST body. RPC method, parameters,
JSON-RPC ID, result and errors are encrypted. Initially disallow notifications
and batches: each accepted command has one response. Use explicit versioning,
strict parsing, bounded bodies/concurrency/timeouts, and structured errors.
Never put secrets in URLs, HTTP headers, logs, tracing or error messages.

Application methods:

| Method | Parameters | Result / semantics |
| --- | --- | --- |
| `allocate_ip` | client UUID, operation UUID | Stable inner IP and subnet metadata; repeated calls return existing mapping. |
| `create_udp` | client UUID, inner IP, operation UUID | Opaque allocation ID, server generation, UDP port, fresh 32-byte transport secret, MTU, startup/inactivity timeouts and timing policy. |
| `release_udp` | client UUID, allocation ID, operation UUID | Idempotently retire this allocation only; already released is success. |

The configured control host determines the transport host unless an explicit
authenticated server address is returned. Never use the HTTP source address as
the client's UDP address. UDP source admission must allow configured roaming;
do not pin a firewall rule to the first observed public client IP.

Define errors for malformed/unauthenticated requests, stale generation,
idempotency conflicts, address conflict/pool exhaustion, allocation quota,
port pool exhaustion, firewall unavailability and temporary server failure.
Separate unauthenticated envelope errors from encrypted application errors.

### Random HTTP paths and content-based dispatch

Do not register a recognizable RPC URL or use the HTTP path to select a control
operation. Accept a valid control POST at any syntactically valid origin-form
path within the configured request-target bounds. Authenticate and validate the
body, then dispatch its encrypted message kind to bootstrap or JSON-RPC handling.
Both the bootstrap exchange and application RPCs follow this rule; neither has
a fixed public discovery path. Do not interpret path segments as filesystem
paths, client identifiers, versions, secrets or RPC methods.

For every HTTP request, the client generates a fresh CSPRNG-based path containing
random ASCII alphanumeric segments separated by `/`. Initial generation policy:
2 to 5 nonempty segments and 24 to 64 total path bytes, including the leading
and separating slashes. Choose segment lengths within those bounds and randomize
the separator positions. Do not add a stable prefix, UUID, operation token,
query string, fragment or percent encoding. Regenerate the path on retries;
idempotency remains bound to the encrypted operation UUID, not the URL. These
are client-generation bounds, not a server requirement that only random-looking
paths are accepted. Set a separate bounded server request-target limit.

Invalid or unauthenticated content receives the same generic HTTP 404 response,
with a small fixed body and no RPC, authentication or version diagnostics.
Use this response for unsupported methods and invalid envelopes, failed AEAD
authentication, or malformed decrypted control/JSON-RPC structure when HTTP
framing permits a response. Do not redirect, advertise an RPC route, provide an
unauthenticated challenge banner, or distinguish unknown paths. Grossly malformed
HTTP or exhausted resource limits may require closing the connection; preserve
strict parser, body-size, timeout and concurrency limits before expensive work.

After content authentication and structural validation, legitimate application
errors (including stale challenges, address conflicts and unknown RPC methods)
use encrypted responses under the control protocol. This preserves recoverable
errors for valid peers without exposing them to unauthenticated probes. Bind
AEAD purpose to the encrypted message kind/protocol context, not a fixed HTTP
path; bind responses to the request token/content as already required.

This removes a fixed URL signature; it does not make the service impossible to
identify. Plain HTTP metadata, ciphertext shape, timing and generic responses
remain observable. Do not claim indistinguishability from an ordinary website.
Add tests that the same valid request works at unrelated short paths, bootstrap
and RPC retries change paths without changing operation semantics, and invalid
requests return the same 404 regardless of their path or authentication failure.
Verify randomized client paths meet length/segment bounds without testing exact
random output. Audit logs must not expose body secrets or add response hints.

### Envelope, replay and retry contract

Use a maintained XChaCha20-Poly1305 implementation, HKDF-SHA256 purpose/direction
separation, and the existing configured PSK source. Do not invent a cipher.
The control envelope uses fresh random 24-byte nonces; its nonce design is
independent of the counter-based UDP format. Authenticate protocol version,
server identity, endpoint purpose and direction, and bind responses to a random
request token plus the exact request. No plaintext RPC error may masquerade as
an authenticated response. No forward secrecy is required.

Before mutations, obtain a short-lived server challenge through an encrypted
HTTP bootstrap exchange. This transport exchange is not an additional address
or allocation RPC. Its request has no application side effect; its response
echoes the client's fresh request token and supplies a random boot generation,
challenge and validity interval. Bind subsequent command envelopes to that
generation/challenge. After restart, reject previous generations before any
mutation. Expire challenges and bound their storage; cap the deduplication
cache without evicting unexpired replay records to admit more work. Reject new
work at capacity. Use monotonic time for live deadlines.

Specify the exact encoding, HKDF labels, challenge consumption rules and cache
lifetimes in phase 1. An authenticated duplicate may return a cached response
without executing again. Never accept a consumed request as a new command.
HTTP connection reuse is optional; reconnecting must not reset replay state.

Idempotency is separate from envelope replay protection. A retry uses a fresh
envelope/request token with the same operation UUID. Serialize concurrent
duplicates. Identical create operations return the same allocation and secret
while live; changed parameters under the same operation UUID are an error.
Released/expired operations return a terminal result, not a newly allocated
port. Retain bounded tombstones for the published retry horizon; reject expired
operations rather than accidentally re-executing them. Include operation age
and server generation in this contract. A new server generation invalidates
old allocation operations; the client deliberately starts a new operation.

Treat an HTTP timeout as an unknown outcome, not a failed mutation. Allow a
reconnected client to recover the original result through an idempotent retry.
Redact cached secrets from diagnostics and clear them when no longer needed.

## UDP authenticated framing

For each allocation generate a fresh OS-random 32-byte transport secret. Derive
independent client-to-server/server-to-client AEAD keys and 16-byte nonce
prefixes using versioned, distinct HKDF labels. Bind derivation to the allocation
identity and protocol context. Never share an allocation's secret with another
allocation, even when a numeric port is reused.

```text
RTP header (12) | packet number (8, big endian) | encrypted body | AEAD tag (16)
nonce = directional prefix (16) | packet number (8)
```

Authenticate the full visible header and packet number, plus fixed version and
allocation context. Keep the current RTP header conventions. The packet number
is independent of the wrapping RTP sequence/timestamp and starts at zero under
each fresh directional key. All data and control packets share that direction's
counter. Never wrap it, reset it under a reused key, or create two independent
transmitters sharing it. Restart requires a fresh allocation/key.

Carry inner IPv4 with its allocation-associated address omitted, as specified
below; reserve non-IP leading values for bounded, explicitly encoded control
types. Reserve the IPv6 version nibble for future IPv6 data, not control.
Pin all encodings and vectors before
runtime integration. Authentication/replay failures have no route, endpoint,
lease, liveness or buffering effects. A 1024-packet receive bitmap per direction
accepts unseen reordered packets and rejects duplicates/old packets. Only a
successfully authenticated packet may advance the window.

Framing adds 36 bytes above UDP, or 64 bytes including outer IPv4 and UDP.
Omitting four inner address bytes makes the net data overhead 60 bytes above
the original inner packet: 4 bytes more than the present format. A 1200-byte
inner IPv4 packet produces 1260 outer IPv4 bytes (1280 over outer IPv6, if
supported). Control bodies do not receive this address compression and add
their own bytes to the full framing overhead.
Verify lengths and bounds rather than assuming the old constants still apply.

### Allocation-associated IPv4 address compression

For client-to-server data omit the four source-address bytes at original IPv4
offsets 12..16. For server-to-client data omit the four destination-address bytes
at original offsets 16..20. The remaining bytes retain their original order;
the version/IHL byte stays first. Each allocation binds exactly one client inner
address, including static addresses, so direction plus allocation state makes
the omission unambiguous. Preserve the remote endpoint's address.

Validate the full original packet and its client-associated address before
compression/encryption. After authentication/decryption, reconstruct the missing
address from allocation state before parsing/validating or writing to TUN. Bound
the compressed input length and account for the four missing bytes when checking
IHL, IPv4 total length and MTU; never pass a compressed packet to the ordinary
IPv4 parser. Preserve IPv4 options, fragments and all remaining bytes exactly.
The original total-length field and IP/transport checksums are unchanged and
remain valid after exact reconstruction; this is header compression, not NAT.
Do not silently replace a mismatching source/destination address during encoding.

No compression flag or per-packet address selector is needed for this matched
protocol version. Additional client addresses or routed source prefixes require
an explicit future protocol change. Add byte-exact roundtrip tests in both
directions, minimum and maximum lengths, IPv4 options/fragments, malformed IHL
and length fields, wrong allocation/address, and preserved checksums. Update
wire vectors, overhead constants, socket bounds and reports accordingly.

### IPv6 scope and future design

Inner IP family and outer transport family are independent: IPv4 inner traffic
can use an IPv6 UDP path without adding inner IPv6 support. Do not infer inner
address allocation or compression from the HTTP/UDP socket's address family.

This implementation remains inner-IPv4-only. Explicitly reject/count inner IPv6
at packet boundaries and reject IPv6 address requests as unsupported. Keep
existing scoped routing guards; do not disable IPv6 host-wide. A future
full-tunnel configuration must either carry IPv6 or explicitly block its bypass
within the protected scope; IPv4-only routing alone does not protect IPv6.

Recommendation for a separately authorized dual-stack phase: persist UUID to
IPv4/IPv6 assignments, bind both client addresses to the same UDP allocation,
dispatch by destination family/address, and distinguish data by the retained IP
version nibble. Omit the 16-byte IPv6 source address upstream and destination
address downstream, reconstructing the original packet before validation/TUN.
Preserve extension headers and transport checksums; IPv6 has no header checksum.
Carry ICMPv6, including Packet Too Big, and define bounded extension-header and
fragment handling. Do not assume IPv4 header parsing applies to IPv6.

Before enabling inner IPv6, replace the current 1200-byte inner MTU policy:
IPv6 requires a link MTU of at least 1280 bytes, and encapsulation requires
additional outer-path headroom or a deliberate below-IPv6 fragmentation/
reassembly solution. Specify path-MTU/error handling and test it before claiming
support. Do not quietly enable IPv6 on a 1200-byte TUN. This future work is not
an implementation gate for the IPv4 core plan.

## Allocation and firewall lifecycle

Lifecycle: preparing -> awaiting path -> ready -> active -> draining -> released.
Failure/expiry can retire any live state. Allocation ID and firewall generation
remain distinct from the numeric port.

1. Validate UUID/address consistency and limits; reserve an allocation slot.
2. Randomly select/bind an available port from the configured pool. Generate
   keys and prepare its owned runtime firewall admission through the supervisor.
3. Verify admission and commit the allocation before returning success. A
   partially completed or cancelled create must be rolled back/reconciled.
4. Require first authenticated client UDP traffic within 30 seconds of creation.
5. Expire after 120 seconds without fresh authenticated client traffic. Valid
   path messages, PINGs and data refresh activity; malformed/replayed packets,
   outgoing server traffic and repeated cached HTTP responses do not.
6. Release/expiry removes route ownership, cancels queues/tasks and retires
   exactly this generation's firewall rule, socket and key. Keep the socket
   reserved while rule removal is uncertain; never reuse it prematurely.

Normally allow two live allocations per UUID. Handle abandoned candidates by
idempotent recovery/release/expiry rather than unbounded third allocations.
Set global allocation and operation/challenge limits. Preserve the existing
firewall reload, unavailable-backend, crash and independent cleanup contracts.
Do not issue host-wide reloads, expose the whole pool or adopt unrelated rules.

## Path validation, roaming and activation

UDP control minimally needs PATH_OPEN, PATH_CHALLENGE, PATH_RESPONSE,
PATH_READY, ACTIVATE, ACTIVATED, PING and PONG (names may change, semantics may
not). Use random challenge/probe tokens bound to allocation, source tuple and
attempt. Keep messages small and cap responses on unvalidated paths.

An authenticated packet from a new outer tuple creates a bounded candidate,
not an immediate downstream redirect. Challenge that tuple; a matching fresh
response proves return reachability. PATH_READY confirms validation to the
client. The same exchange handles initial establishment and NAT rebinding.
Data from an unvalidated candidate is dropped until validation completes.
The old validated path remains usable while probing a candidate.

Only an explicit, acknowledged activation changes downstream ownership. Use a
monotonic client activation generation across its live allocations, allocated by
one client state owner. Older activation requests cannot reclaim ownership.
Repeated activation of the same target/generation returns its status without
changing state again. Once superseded, an old allocation cannot reactivate,
even through delayed messages. A restarted client starts a fresh control session
and allocations; define recovery of stale leases without reusing keys/counters.

Lost ACTIVATED replies must be recoverable: server remains on the committed
route, client keeps both receive sockets open, and retries the same logical
activation with fresh packet numbers. A delayed reply cannot activate a newer
replacement candidate. Endpoint changes within an active allocation also need
ordered path attempts; accepting a reordered packet never restores an old tuple.
Pin exact state transitions and token consumption rules in phase 1.

## Client rotation and recovery

Rotate on a 300-second activation-to-activation schedule. Begin preparation
early (initial lead time 10 seconds) so a healthy switch occurs near the deadline.
The client owns the schedule; the server never announces a replacement port.

1. Create the next allocation while old traffic continues.
2. Obtain a fresh outer socket through the host socket provider and validate
   the candidate. Keep old-path traffic and liveness running.
3. Open reception on both paths and activate the candidate explicitly.
4. After activation confirmation, send new upstream traffic through the new
   allocation. Continue receiving both; server accepts authenticated old-path
   upstream traffic during a 10-second drain grace.
5. Release the old allocation after grace. Its old traffic cannot affect the
   new allocation's endpoint, activation or lease.

No duplicate downstream data transmission by default. Handover is not a
zero-loss guarantee. Preserve application packet order within bounded queues;
do not pause all traffic for an HTTP round trip. If preparation fails, retain
the established allocation and retry with capped exponential backoff/jitter.
Never retire a working allocation merely because a rotation deadline passed.

On network-change notification or failed liveness, try a fresh socket/path on
the existing allocation, then recreate the allocation through HTTP if needed.
Recognize server-generation changes and discard stale allocation assumptions.
The HTTP endpoint and every new UDP socket must bypass the tunnel; losing the
data path must not prevent its own recovery. Preserve scoped routing guards.

## Keepalive and health policy

The client sends an authenticated PING after 15 seconds without outbound UDP
traffic, sampled with +/-20% jitter. The server sends PONG echoing the probe
identifier and never acknowledges a PONG. Incoming-only traffic does not suppress
the client's NAT maintenance. Healthy bidirectional traffic suppresses probes.

Also initiate a health probe after 15 seconds without authenticated inbound
traffic even if outbound application data continues. Allow one outstanding
liveness probe per path. Initial timeout is 1 second, then two retries with
2-second and 4-second waits (three transmissions, about seven seconds total).
Each transmission has fresh packet number and probe ID; only its matching PONG
completes that attempt. Any fresh authenticated inbound traffic clears general
dead-path suspicion; only an explicit matching response completes path validation
or activation. Do not let ordinary traffic postpone an outstanding probe forever.

After exhaustion mark the path suspect and start bounded path repair while
retaining old reception. Recovery failures back off; they do not spin or create
unbounded allocations. Expose these defaults as validated configuration with
timer consistency checks. Server inactivity timeout must exceed the normal
keepalive/retry budget. These defaults require path validation, not a guarantee
for every NAT. A server may expire inactive unresponsive clients.

## TUN dispatch, queues and concurrency

One logical TUN reader looks up destination inner IP in the active-route table
and enqueues to that allocation. Never have independent allocation tasks read
and discard packets from a shared TUN. Allocation tasks receive/authenticate UDP,
track validated endpoints, and submit validated inner packets to the TUN writer.
Check inner packet structure, MTU and the claimed inner source address for
configuration consistency. Preserve existing destination forwarding policy.

Before the first validated/active path, route packets to a single bounded
per-client pending queue. During rotation retain one downstream route/queue
owner; do not duplicate queues per allocation. Defaults: maximum age 1 second,
256 packets or 256 KiB per client (whichever is reached first), and 16 MiB
global buffered payload budget. Drop expired packets first, then oldest packets
under pressure, and count each reason. Drop unknown destinations.

After activation, drain unexpired queued packets before newer arrivals through
the same FIFO. Use bounded send batches (initially 32 packets per scheduler
turn), yield fairly and respect socket backpressure. Do not send an unlimited
reconnection burst. Bound socket/TUN write queues too. Release/expiry cancels
tasks and drops their buffers without late writes to retired descriptors.

Serialize address assignment, activation and allocation state transitions under
one logical owner; avoid locks held across HTTP/firewall I/O. Use bounded channels
and generation checks on asynchronous completions. Define who owns each socket,
key, route entry, queue and cancellation token. Late prepare/release completions
cannot mutate a newer allocation. Retain observable readiness/degraded states.

## Implementation phases and acceptance gates

### 1. Freeze executable protocol contracts

Write a replacement protocol specification with exact wire formats, HKDF labels,
HTTP challenge/envelope rules, RPC schemas/errors, deadlines and state diagrams.
Record all defaults above and any justified deviation. Produce fixtures/vectors
for both directions and an explicit restart/retry table. Do not start with a
second legacy mode or format negotiation. The first implementation is a matched
client/server breaking change; reject old frames clearly.

### 2. Implement codec and deterministic state machines

Implement AEAD, counters/replay windows, address allocation, operation
deduplication, leases, path validation, activation and client timers without real
firewall/network dependencies. Inject clock/RNG/I/O as appropriate. Cover bit
tampering, wrong keys/directions/context, truncation, replay-window boundaries,
reordering, counter exhaustion, lost/duplicate/late messages and restart rules.
Validate byte counts and independent known-answer vectors.

### 3. Add HTTP service and supervised allocation lifecycle

Add the bounded listener/client and persistence. Integrate port-admission via
existing privileged boundaries. Cover concurrent duplicate creates, response
loss after commit, cancellation at each prepare boundary, pool exhaustion,
static/dynamic address collision, malformed input, cache pressure, replay after
restart, lease expiry and stale release after numeric-port reuse. Assert no
unprepared allocation is returned and failed preparation preserves old traffic.

### 4. Integrate Linux transport, rotation and buffering

Connect the single TUN dispatcher and per-allocation tasks. Add HTTP bypass to
the host-network socket path, fresh UDP sockets, fair bounded queues and
client-driven rotation/repair. Test two clients concurrently, same-UUID handover,
roaming, endpoint rollback attempts, one-way traffic, active-upload blackholes,
idle probe suppression, lost ACTIVATED replies, failed rotation and queued
packet expiry/order. Use short test-only timers while keeping production defaults.

### 5. Validate on disposable Linux fixtures

Run focused Rust tests and relevant workspace compilation checks. Use the
existing authorized Linux validation client identified in repository/user
instructions; do not invent another host or request that choice again. Its
login shell requires `sh -lc` and adding the Cargo bin directory to PATH.
Stage only task files in private temporary directories. Generate private keys
outside the checkout. Use disposable unprivileged user/network namespaces for
TUN, loss/reorder, NAT rebinding, process death and application TCP/UDP checks.

Exercise actual firewall mutation only where the existing isolated fixture has
the required privileges; do not claim unprivileged tests prove firewalld behavior.
Run the isolated real-firewalld lifecycle/reload/crash suite when available,
otherwise report that gate as unverified. Preserve unrelated services/rules.
No public/cross-host validation or deployment is implied by this plan. Remove
task processes, namespaces, temporary keys/staging and owned rules afterward,
and verify cleanup. Never put actual hosts, usernames or private paths in tracked
plans, scripts or reports.

### 6. Remove superseded paths and finish documentation

Remove base-port settings/keys, server rotation announcements/epochs, STATE
secret distribution and base fallback from the implemented core. Update CLI,
READMEs, protocol/firewall documents and fixture expectations to the final
architecture. Preserve historical result reports as historical evidence; do not
rewrite their measurements as new acceptance. Record mobile integration as a
separate follow-up with precise shared-API implications.

## Required validation report and completion criteria

The implementing agent must report commands/results, defaults, wire overhead,
remaining limitations and cleanup evidence. This plan's PR is documentation
only; do not claim any new protocol behavior has been validated yet.

Completion requires demonstrated dynamic/static addressing, idempotent create
and release, restart-safe control replay protection, authenticated/replay-checked
data, acknowledged path activation, five-minute client-owned rotation, bounded
pre-path buffering, idle-only NAT maintenance plus active-path health checks,
HTTP recovery outside the tunnel, and generation-safe firewall/socket cleanup.
Document any unavailable environment-specific gate explicitly rather than
marking it passed. Do not expand the work into mobile delivery or deployment.

## References

- [Current protocol](rtp-l3-protocol.md) and
  [firewalld lifecycle](rtp-l3-firewalld-design.md): existing implementation.
- [JSON-RPC 2.0](https://www.jsonrpc.org/specification): message structure;
  application idempotency/replay behavior is specified by this plan.
- [Libsodium XChaCha20-Poly1305](https://doc.libsodium.org/secret-key_cryptography/aead/chacha20-poly1305/xchacha20-poly1305_construction): AEAD and nonce uniqueness.
- [RFC 6479](https://www.rfc-editor.org/rfc/rfc6479.html): replay-window technique.
- [RFC 9000 path validation](https://www.rfc-editor.org/rfc/rfc9000.html#section-8.2):
  reference for return-reachability validation, not adoption of QUIC.
- [RFC 8085](https://www.rfc-editor.org/rfc/rfc8085.html): UDP transport and
  middlebox considerations.
- [RFC 8200](https://www.rfc-editor.org/rfc/rfc8200.html): IPv6 headers and
  minimum link MTU; [RFC 4443](https://www.rfc-editor.org/rfc/rfc4443.html): ICMPv6.
