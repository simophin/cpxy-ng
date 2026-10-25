# Experimental RTP-framed L3 tunnel protocol

This is the single protocol implemented by `experiments/rtp-l3`. There are no
legacy modes, format negotiation or production compatibility commitments. Both
peers must use the same current build. Historical experiment reports describe
measured configurations; they are not alternative protocol specifications.

The tunnel carries kernel IPv4 packets in RTP-framed UDP. It is not codec media
or standardized SRTP, and its packet shape does not establish censorship
resistance. Desktop/mobile integration, production deployment, cover traffic,
FEC, outer reliability and fragmentation are outside the current scope.

## Security and scope

A private 32-byte shared PSK supplies directional encryption and control keys.
Data uses raw XChaCha20 without an authentication tag or replay protection.
This is an accepted experimental choice: end-to-end application TLS still
protects TLS content, but the tunnel does not authenticate data, prevent data
replay, or provide forward secrecy. A modified/replayed packet that parses as
IPv4 can be forwarded. Header tampering changes the data keystream.

Control has a truncated HMAC-SHA256 tag and live-session sequence/epoch checks.
Authenticated PING alone creates or moves a session. Authenticated PROBE tests
a candidate return path without moving data routing. All clients share the PSK;
any key holder can impersonate another client. An on-path attacker can race a
fresh PING from another tuple. Captured STATE secrets can be recovered by a
later PSK holder, so rotating secrets does not provide forward secrecy.

## Framing and encryption

```text
data:    IPv4 | UDP | RTP (12) | random (16) | encrypted inner IPv4
control: IPv4 | UDP | RTP (12) | random (16) | encrypted control body | tag (16)
```

The RTP header is exactly 12 bytes: byte 0 `0x80` (standard RTP version 2,
no extension/CSRC), byte 1 `96` (marker zero), sequence (16 bits), timestamp
(32 bits), SSRC (32 bits). Fields are network byte order. Per transmit stream,
sequence, timestamp offset and SSRC start randomly. Sequence increments per
packet; timestamps advance from a monotonic clock at 90 kHz. They wrap normally.
Each newly installed server-port key gets a fresh transmit stream.

The XChaCha20 nonce is 24 bytes:

```text
RTP bytes 2..10 (8 bytes: sequence, timestamp, SSRC high half) | random (16)
```

The 16 random bytes come from the OS CSPRNG for every packet. They are sent in
clear. A fresh raw XChaCha20 cipher starts at block counter zero per packet,
matching libsodium's `crypto_stream_xchacha20_xor` convention. Nonce safety
rests on fresh random bytes, not RTP counters; XChaCha20 is not nonce-misuse
resistant. Packet loss, reordering, rollover and process restarts do not require
implicit crypto counters or replay windows.

Data ciphertext length equals inner IPv4 length. Inner MTU is 1200 bytes;
outer IPv4 overhead is 56 bytes (20 + 8 + 12 + 16), so maximum outer length is
1256. A 1000-byte inner UDP application payload produces 1084 outer IPv4 bytes.
Validate the fixed RTP prefix, packet bounds and decrypted IPv4 header/total
length. These checks do not establish authenticity.

## Keys

All outputs below are 32-byte HKDF-SHA256 outputs. There are distinct keys by
purpose and direction. For base-port keys, input key material is the PSK and
salt is absent (RFC 5869 zero salt).

| Purpose | Exact HKDF info |
| --- | --- |
| Base data, client → server | `rtp-l3-poc/data/client-to-server` |
| Base data, server → client | `rtp-l3-poc/data/server-to-client` |
| Base control MAC, client → server | `rtp-l3-poc/control/client-to-server` |
| Base control MAC, server → client | `rtp-l3-poc/control/server-to-client` |
| Port-key salt from PSK | `rtp-l3-poc/port-salt` |

Each bound rotation port gets a fresh OS-random 32-byte secret, including when a
numeric port is reused. Rotation HKDF input is that secret; its salt is the
PSK-derived port-key salt above.

| Purpose | Exact HKDF info |
| --- | --- |
| Rotation data, client → server | `rtp-l3-poc/port/data/client-to-server` |
| Rotation data, server → client | `rtp-l3-poc/port/data/server-to-client` |
| Rotation control MAC, client → server | `rtp-l3-poc/port/control/client-to-server` |
| Rotation control MAC, server → client | `rtp-l3-poc/port/control/server-to-client` |

The directional data key encrypts both data and control bodies. The independent
control key authenticates control only. STATE carries the current port's secret
over the session's existing path, including in replies to PING. The receiving
server socket or server source port selects keys; no key hint or trial
key-decryption is used. Base keys remain available for bootstrap/fallback.
Retired port secrets/streams are discarded. Port keys are shared across clients,
not individualized per client.

## Control messages

The decrypted body's first byte distinguishes data (high nibble 4) from control
(types in `0x01`–`0x0f`; the three below are implemented). Other values are
rejected. Control uses this body, with big-endian fields:

```text
type (1) | reserved zero (1) | body length (2) | fields | zero padding
```

The reserved byte must be zero; it is not a version selector. Body length covers
the four-byte header and fields, excluding padding and tag. Nonzero padding or
reserved bytes, unsupported types, wrong lengths or bad tags are rejected.

```text
tag = HMAC-SHA256(control_key, RTP header || random || encrypted body)[0..16]
```

Encrypt the body with the directional data key, then append the tag in clear.
The complete visible header and ciphertext are MAC-covered. No state mutation
occurs until control authentication and direction checks pass.

| Type | Direction | Fields after header | Body bytes |
| --- | --- | --- | ---: |
| `0x01 PING` | client → server | session (8), inner IPv4 (4), sequence (4), known server instance (8), known epoch (4) | 32 |
| `0x02 STATE` | server → client | session (8), echoed sequence (4), instance (8), epoch (4), current port (2), port secret (32), base port (2), grace seconds (2) | 66 |
| `0x03 PROBE` | client → server | same fields as PING | 32 |

A zero echoed sequence means unsolicited STATE. Each control packet chooses a
random padded outer IPv4 size between its minimum and 260 bytes inclusive.
Minimum outer sizes are 104 bytes for PING/PROBE and 138 for STATE. Size overlap with
small data packets does not make this valid media or hide idle cadence.

## Sessions, replay checks and routing

A client starts with a random 64-bit session ID, its configured inner address,
and an increasing 32-bit PING sequence. Sequence exhaustion requires a new
client process/session; every retry uses a fresh sequence. The server starts
with a random 64-bit instance ID and epoch zero. Each successful rotation
increments the epoch. Runtime timers use monotonic whole seconds and are driven
at least once per second.

The server table is capped at 1024 sessions. Each entry holds its inner address,
receiving port/peer tuple, latest PING sequence, last-seen time, acknowledged
epoch and announcement retry state. PING must authenticate, use a strictly newer
sequence, name an inner address inside the server CIDR other than the server's,
and arrive on an installed port. A conflicting inner address replaces the old
session and increments the conflict counter; an ambiguous tuple shared by
sessions with different inner addresses is rejected. PING alone learns/moves
the tuple after NAT rebinding or roaming.

Upstream data must arrive on a live session's registered path. It cannot create
or move the session. A downstream TUN destination selects the session and its
return path; unknown destinations are dropped. Idle sessions expire after
90 seconds without valid incoming traffic. Expiry forgets sequence history;
control replay suppression is scoped to live state, not a persistent database.

A client checks the server IP and known source port before decoding. It accepts
a changed instance only when STATE echoes its latest outstanding PING; that
echo is consumed once. For the same instance, older epochs are rejected;
equal-epoch STATE cannot change the port/secret. Equal-epoch unsolicited or
replayed STATE is a no-op: it cannot reset retries or extend grace. A fresh
solicited equal-epoch reply can complete liveness without switching. Higher
epoch STATE installs the new path and prompts an immediate PING there.

## Rotation, keepalive and fallback

The server keeps the base socket open and immediately binds an initial random
rotation port. Default rotation mean is 300 seconds with ±50% jitter; CLI
values must be at least two seconds. Explicit zero disables subsequent
scheduled rotation for a matched fixed-port baseline. The range must contain
at least three non-base ports. Binding retries occupied candidates; a failed
bind leaves epoch/session state intact and is counted.

A successful rotation binds first, generates the fresh secret, advances epoch,
and announces through each session's existing path. Retries occur at +1, +2
and +4 seconds until a PING on the new port acknowledges instance and epoch.
The server never relies on unsolicited traffic from a new port opening NAT.
The client first sends authenticated PROBE on a fresh host-network socket for
the advertised port. The server requires an existing session, matching inner
address/current instance/epoch and a strictly newer sequence; it advances replay
state and replies with STATE from the probed port, while preserving the
registered data path and announcement retries. Only a matching fresh candidate
echo with the advertised instance/epoch/port/secret can validate that socket.
The client then switches and sends registration PING before data on that socket.
Only PING commits the server route. Data may overtake registration and be dropped.

Candidate challenges use three attempts with fresh sequences and full one-second
monotonic deadlines. Exhaustion replaces only the candidate socket and retries;
traffic and keepalives continue on the established path. Newer advertisements
replace older candidates. Old candidate echoes cannot validate a replacement.
Candidate challenges share the session sequence space with PING; active PING
completion takes priority. No candidate may create or displace a server session.

At most base/current/previous sockets remain installed. A previous socket
retires only after both 60-second grace and its last session leaving. A later
rotation can evict the oldest socket early to keep the limit. Clients stranded
there must recover through base. Reusing a numeric port replaces its secret;
the client does not retain ambiguous previous keys for that same port.

The launcher retains a private host-network socket broker and passes its Unix
`SOCK_SEQPACKET` channel into the TUN/app namespace. It creates fresh bound UDP
sockets and transfers their descriptors with SCM_RIGHTS; payloads and keys never
traverse this channel. The runtime retains at most base, active, previous and
candidate sockets, selected by server port. It keeps the working data path while
probing a candidate and drops retired descriptors. Candidate retries and base
recovery can replace unusable mappings without restarting the TUN/apps.
The optional launcher bind port selects the initial bootstrap socket only.

The initial connected host socket is disconnected with AF_UNSPEC, restoring its
original local port if Linux clears the implicit binding. Broker sockets are
unconnected from creation and preserve their host network namespace. Fresh local
ports do not establish a particular NAT implementation or bypass every failure.
Client TUN activation waits for candidate validation and a completed registration
PING; applications start after that confirmed path is usable.

PING is sent at startup before data, and when either send or receive activity
has been idle for the sampled keepalive interval. Default mean is 15 seconds
with ±20% jitter; `POC_KEEPALIVE_SECS` accepts 5–45. Jitter plus retries must fit
fixed grace. These are runtime settings, **not validated recommendations for
the measured public path**. Under steady bidirectional traffic idle PINGs are
suppressed. Valid control/data refresh the corresponding liveness state.

Each PING expects STATE within two seconds, for three attempts total. Missing
all three selects base; probes there back off 1, 2, 4, 8, 16, 30, 30… seconds
until valid STATE advertises a candidate and fresh PROBE/STATE validation
restores the rotation path. Recovery attempts renew the base socket rather than
reusing a persistently unusable return tuple. Data continues during retries
and may use base during fallback. Base therefore carries bootstrap/fallback
data as well as control. The server responds to PING and announces rotation;
it does not originate periodic keepalives. Cover traffic is absent.

## Runtime and acceptance

The Linux runtime polls nonblocking TUN/UDP in bounded 64-packet batches,
requests 4 MiB socket buffers subject to host limits, and counts local drops.
It reports `protocol: "rtp-l3"`, readiness, five-second counters and final
counters on graceful termination. CLI, counters, fault hooks and validation
commands are in the [crate README](../experiments/rtp-l3/README.md).

The [handoff](rtp-l3-poc-handoff.md) owns acceptance status and next work.
Public-path idle retention remains failed; consolidated naming/key labels and
the reserved-zero header do not fix that failure. Historical measurements used
earlier experimental key labels/header markers and are not a cross-host test
of the consolidated wire bytes. No old-mode decoder or migration path exists.
