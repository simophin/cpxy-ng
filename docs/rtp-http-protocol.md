# HTTP-controlled RTP transport v2

Status: frozen contract for implementation; see the progress checklist in
[rtp-http-control-plan.md](rtp-http-control-plan.md). This is a matched
client/server breaking change, with no negotiation or legacy fallback.
The existing deployed v1 implementation is not evidence that v2 works.

## Cryptographic context

All integers in binary encodings are unsigned big endian. IDs and tokens are
16 random bytes. JSON IDs use canonical lowercase hyphenated UUID strings.
Secrets are 32 bytes; JSON binary fields use lowercase hexadecimal, with exact
length validation. All randomness comes from the operating system CSPRNG.
The configured server identity is a nonempty UTF-8 string of at most 255 bytes,
provisioned identically at each peer (not inferred from Host or a source IP).
PSK derivation remains Argon2id with the existing v1 password salt/parameters.

HKDF is HKDF-SHA256. UDP uses the allocation ID as salt and the transport secret
as input key material. Expand 48 bytes with info
`cpxy-rtp/v2/udp/client-to-server` or
`cpxy-rtp/v2/udp/server-to-client`: first 32 bytes key, last 16 nonce prefix.
Control uses server identity bytes as salt, PSK as input material, and 32-byte
expansion with info `cpxy-rtp/v2/http/client-to-server` or
`cpxy-rtp/v2/http/server-to-client`.

All encryption is XChaCha20-Poly1305 (16-byte tag). Keys/counters are never
persisted. Allocation identity is never a port number.

## HTTP envelopes and bootstrap

HTTP/1.1 POST, Content-Length required, no transfer encoding, compression,
notifications, batches or pipelining. Close after each response. Headers are
bounded to 8192 bytes and 32 fields, body to 16384 bytes, request target to 2048
bytes, connection deadline to 5 seconds, concurrent connections to 64.
Accept any syntactically valid origin-form path; no path dispatch. Clients
choose a new random ASCII alphanumeric path for every request/retry: 2..5
segments, 24..64 bytes including slashes, with random separator positions.

Binary body: `version(1)=2 | kind(1) | nonce(24) | ciphertext | tag(16)`.
Kinds: bootstrap request=1, bootstrap response=2, command=3, response=4.
AAD is `cpxy-rtp/v2/http\0 | identity-length(1) | identity | version | kind`.
The nonce is fresh OS randomness on every encryption. Direction and kind must
match before decrypting. The JSON plaintext has no unrecognized fields.

Bootstrap request: `{"token":HEX16}`. Response:
`{"token":HEX16,"request_hash":HEX32,"generation":HEX16,
"challenge":HEX16,"valid_for_ms":30000}`. Hash is SHA256 of the complete
request envelope, including nonce and tag. A response must match both token
and hash. Bootstrap has no application side effect. Cap live challenges at
4096; do not evict live challenges to admit new ones. Bootstrap replays may
consume challenge capacity but cannot authorize mutations without a fresh
command tied to the challenge. Reuse of the same bootstrap envelope returns
its cached response while live, without creating a second challenge.

Command plaintext: `{"token":HEX16,"generation":HEX16,"challenge":HEX16,
"rpc":JSON_RPC_REQUEST}`. The command is bound to one outstanding challenge.
A challenge is consumed by the first authenticated structurally valid command
(including an application error). Atomically cache its exact request hash and
encrypted response until challenge expiry. An exact duplicate returns that
response; another command against a consumed challenge returns encrypted
`replayed_challenge`. Expired/missing challenges return encrypted
`stale_challenge`; previous generations return encrypted `stale_generation`.
Validation and cache reservation precede any mutation. No cache entry is evicted
before expiry. Capacity failure is recoverable, without mutation.
Response plaintext: `{"token":HEX16,"request_hash":HEX32,"rpc":JSON_RPC_RESPONSE}`.

Unsupported HTTP methods, invalid envelopes/authentication, and malformed
plaintext/JSON-RPC structures all receive exactly HTTP 404 with body `Not Found\n`.
No diagnostic headers. Gross framing violations/resource exhaustion may close.
Authenticated application errors use HTTP 200 and kind 4 encrypted responses.
RPC requests must have `jsonrpc:"2.0"`, non-null string ID (1..128 bytes),
method string (1..64 bytes), and object params. No extra fields. Unknown methods
with otherwise valid structure are encrypted application errors.

## RPC and idempotency

Every method takes `client_id`, `operation_id`, `generation`, `issued_ms`;
issued_ms is monotonic server age from bootstrap metadata (bootstrap response
also carries `server_age_ms`). Reject future ages and ages older than 600000 ms.
Scope operation IDs to the current server generation. Clients must abandon all
old operations when generation changes, bootstrap again, and recreate leases.

- `allocate_ip`: no other fields. Result `inner`, `prefix`, `server`.
- `create_udp`: additionally `inner` (IPv4). Result `allocation_id`, `generation`,
  `activation_floor`, `port`, `secret`, `mtu:1200`, `startup_ms:30000`, `inactivity_ms:120000`,
  `rotation_ms:300000`, `lead_ms:10000`, `drain_ms:10000`.
- `release_udp`: additionally `allocation_id`. Result `released:true`.

Serialize operation reservation, IP assignment and activation under one owner.
Fingerprint canonical parsed parameters including method and client, excluding
JSON-RPC ID and envelope token. Concurrent identical operations share one
result. A changed method/parameter yields `idempotency_conflict`. Store results
until issued_ms+600000; expired operations are rejected by age even after
cache removal. Maximum 8192 operations, no eviction of live entries. Create
results are replaced by `allocation_terminal` on release/expiry; cached secrets
are removed. Retry release remains success after retirement. Maximum two live
allocations per UUID and 1024 globally, including preparing/retiring allocations.

Error codes: -32601 unknown_method; -32602 invalid_params; -32001 stale_generation;
-32002 stale_challenge; -32003 replayed_challenge; -32004 idempotency_conflict;
-32005 address_conflict; -32006 pool_exhausted; -32007 allocation_quota;
-32008 port_pool_exhausted; -32009 firewall_unavailable; -32010 temporary_failure;
-32011 capacity; -32012 expired_operation; -32013 allocation_terminal;
-32014 unsupported_address_family. Error messages contain no input/keys.

Persist UUID/address mappings atomically (fsync file, rename, fsync directory),
under an exclusive lifetime lock. Reserve usable static claims before create;
never overwrite another UUID or allow one UUID two addresses. Exclude subnet
network, broadcast, server, reservations and existing mappings from dynamic pool.
Never recycle persisted dynamic addresses. Persist client UUID independently of
runtime journals. A copied UUID shares identity; conflicting use reports a quota
or address conflict. A restarted client never resumes old packet counters. Each create returns
`activation_floor`, one greater than the live per-UUID activation high watermark.
The client state owner assigns at least that number, preserving monotonicity
across process restarts without persisting packet counters. Superseded leases
expire at their drain deadline even if the old client died before release.
Quota pressure from other abandoned candidates still requires expiry/recovery.

## UDP encoding

`RTP(12) | packet-number(8) | ciphertext | tag(16)`. RTP bytes 0..2 are
`80 60`, sequence wraps at 16 bits, timestamp is random offset plus monotonic
90kHz ticks, SSRC random per transmitter. All 20 visible bytes are authenticated.
AAD is `cpxy-rtp/v2/udp\0 | allocation-id(16) | RTP | packet-number`.
Nonce is directional prefix(16) followed by packet-number(8). First number zero;
no cloning transmitters, wrapping or resetting. Exhaustion requires a new lease.
One 1024-bit receive replay window per direction; authentication precedes window
advance. Reject frames shorter than 37 or longer than 1232 bytes. IPv4 data
minimum body length is 16; full framing adds 36 bytes and compression removes 4.
1200 inner => 1232 UDP payload => 1260 outer IPv4 / 1280 outer IPv6.

Data retains IPv4 version nibble 4, removes source bytes 12..16 upstream and
destination bytes 16..20 downstream. Verify full IPv4 lengths/IHL/MTU and exact
allocation address before encoding. Reconstruct before ordinary packet parsing.
Preserve options, fragments and checksums byte for byte. Reject inner IPv6.

Control body starts with type (one byte), followed by fields:

| Type | Name | Fields |
| --- | --- | --- |
| 1 | PATH_OPEN | attempt u64, token 16 |
| 2 | PATH_CHALLENGE | attempt u64, token 16, challenge 16 |
| 3 | PATH_RESPONSE | attempt u64, token 16, challenge 16 |
| 5 | PATH_READY | attempt u64, token 16 |
| 7 | ACTIVATE | activation u64, attempt u64, token 16 |
| 8 | ACTIVATED | activation u64, attempt u64, token 16 |
| 9 | PING | probe 16 |
| 10 | PONG | probe 16 |

Exact sizes required; no trailing bytes. Type 4 and 6 are reserved IP nibbles.
Challenges expire after 5 seconds. At most one candidate per allocation, at
most one challenge response per fresh PATH_OPEN packet (<= request+16 bytes).
Attempts increase monotonically across sockets within an allocation. Repeated
opens for the same tuple/attempt/token resend the same challenge; other equal
or older attempts cannot replace the candidate. Only matching tuple, attempt,
token and challenge completes validation. Completion consumes the candidate,
pins the validated tuple, and sends READY. Duplicate matching responses resend
READY. Old tuple data never changes endpoint ownership. Drop unvalidated data.
Invalid or replayed packets have no state effects, including inactivity.

Activation is explicit, from the validated tuple, with matching attempt/token.
One client state owner allocates increasing u64 activation numbers across live
allocations. The server keeps a per-UUID high watermark until all leases expire;
a superseded allocation can never reactivate. Equal number/target resends ACK;
equal number/different target and lower numbers are rejected. Changing the
validated path of an active allocation likewise needs activation before moving
downstream. Lost ACK: server keeps committed route; client retries same logical
activation with fresh packet number, receives on both sockets, and switches
upstream only on matching ACK. Delayed ACK cannot select another candidate.

```text
preparing -> awaiting_path -> ready -> active -> draining -> released
                  |           |         |          |
                  +-----------+---------+----------+--> expired/released
```

By default, create binds UDP port zero and uses the OS-assigned available port.
An explicitly configured UDP pool instead selects a random available port
inside that pool. In both cases, bind precedes admission; the actual nonzero port
is journaled, admitted and returned to the client. Port selection is independent
of the HTTP TCP port, and no whole-range firewall rule is installed.

Admission prepare/commit run outside packet owner. Late completions carry the
allocation ID; retirement holds the UDP socket until generation-scoped firewall
removal succeeds. A failed candidate never alters the established route.
Expiry is 30 seconds from create without any authenticated traffic, then 120
seconds after last fresh authenticated client packet. Outgoing traffic and
cached HTTP responses never refresh it. Old upstream accepted for 10 seconds
after route switch. Superseded paths cannot change new lease state.

## Timers, buffering and restart

Client rotation deadline is activation+300 s; prepare at deadline-10 s, retain
working path on failure, retry with exponential 1..30 s delay and 20% jitter.
Fresh UDP socket for each allocation/repair; HTTP and UDP must use host bypass.
No base UDP listener or server rotation message exists.

Idle PING after 15 s without outbound traffic, sampled +/-20%; independently
probe after 15 s without authenticated inbound traffic. One outstanding probe
per path, transmissions with fresh IDs/counters at 0, 1, 3 s; fail at 7 s.
Only matching PONG completes the probe. Fresh inbound data clears general
suspicion but cannot extend the probe deadline. Never acknowledge PONG.
Network change first attempts fresh-socket path repair on the same lease, then
HTTP recreation with bounded backoff. Replay state survives socket changes.

One TUN reader dispatches by destination to one per-client FIFO. Limits: 1 s age,
256 packets, 256 KiB per client, 16 MiB total payload. Expire first, then drop
oldest under pressure. Unknown destinations drop. Flush FIFO before newer
arrivals, at most 32 per scheduler turn. Bound all write channels. Retirement
cancels outstanding writes and drops buffers before closing descriptors.

| Event | Required behavior |
| --- | --- |
| HTTP response lost after create | New challenge/envelope, same operation; recover same port/secret |
| Exact envelope duplicated | Cached response; no second mutation or refresh |
| Consumed challenge, altered command | Encrypted replay error, no mutation |
| Released create retried | Terminal; no new port/secret |
| Operation older than retry horizon | Reject even if cache record removed |
| Server restart | Fresh generation/challenges; reject old commands/keys |
| Client restart | Fresh allocations/counters; recover UUID/address only |
| Port reused | Fresh allocation/key; stale release/data has no effect |
| Lost activation ACK | Same logical activation, fresh counters; retain both receivers |
| Delayed old path or activation | Never roll route/endpoint backward |

Mobile control HTTP and host-protected TCP sockets are a separate integration.
Until adapted, mobile must report unsupported protocol before opening a tunnel;
it must not send v1 frames to v2 servers. Privileged namespace/firewalld fixtures
are separate acceptance gates; deterministic tests alone do not prove them.
