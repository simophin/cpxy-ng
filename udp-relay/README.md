# Standalone UDP relay

`cargo build -p udp-relay --locked` produces `target/debug/cpxy-udp-relay`.
Run `cpxy-udp-relay run CONFIG.json` as a local administrator/service identity.
Configuration is trusted administrator input, protected through local filesystem
and service-manager permissions. The `run` command has no network management listener. Packet senders cannot choose upstreams. The forwarding engine has no RTP dependency; it does not load RTP, TUN, VPN keys or payload codecs.

Example (documentation addresses):

```json
{
  "mappings": [
    {"id":"echo", "generation":"deployment-1", "listen":"192.0.2.1:30000",
     "upstream":"198.51.100.2:8080", "max_flows":128}
  ],
  "max_flows":256,
  "max_source_flows":32,
  "max_payload":2048,
  "socket_buffer":16384,
  "idle_seconds":180,
  "packets_per_second":10000,
  "bytes_per_second":10000000,
  "mapping_packets_per_second":5000,
  "mapping_bytes_per_second":5000000,
  "source_packets_per_second":1000,
  "source_bytes_per_second":1000000
}
```

Each `(mapping, generation, observed client IP:port)` receives an independent
connected upstream socket. Replies return from the original ingress socket to
the original tuple. Rebinding creates another flow; delayed replies to the old
flow remain assigned to the old tuple. Mapping ports may translate, and a
client may use several mappings independently. Upstreams are numeric IP addresses;
there is no implicit DNS or payload-derived destination. The upstream socket
uses its route-selected local address and ephemeral port. Connected UDP filters
responses from other peers. No upstream connect handshake/readiness is implied.

All listeners bind before `ready`; a bind failure closes the entire partial
allocation. SIGTERM/SIGINT gracefully close sockets and log final counters.
Restart loses flows; new packets recreate them, and application recovery is the
application's responsibility. Shutdown does not drain UDP queues. Static mapping
IDs/generations are opaque inventory metadata; changing destinations requires a
process restart and a new administrator-selected generation. They are a seam for
future authenticated generation-bound listener management, not dynamic leases.
Admission is explicitly administrator-managed: provision scoped firewall rules
externally. This delivery does not mutate host forwarding, NAT or firewall state.

Limits bound global/mapping/source-IP flows, total file descriptors (at most
`max_flows + mappings.len()`), and kernel socket buffers. Linux may cap requested
buffers and normally reports twice the requested size for kernel bookkeeping.
There are at most `max_flows` source accounting records and one shared payload
buffer (`max_payload + 1` bytes); there are no per-flow user-space queues.
Source-IP limits span mappings, so CGNAT clients share the quota. Flow idle time
updates on accepted traffic in either direction. Sources without live flows are
retained until the next rate window, so failed socket allocations cannot reset
the source rate limit. All ceilings are validated and mandatory.

Rates use one-second fixed windows, counting both directions against independent
global, mapping and source ceilings. One request/response normally consumes two
packets and both payload sizes. At window boundaries a short burst may span two
windows. Rate checks precede flow allocation. A poll handles at most 32 ingress
packets per mapping and four responses per flow, then sleeps two milliseconds.
This simple bounded scanner favors small deployments; no high-throughput claim
or cryptographic source identity is made.

`recvmsg(MSG_TRUNC)` rejects entire oversized/truncated datagrams in both
directions, including payload prefixes that otherwise appear valid. Zero-length
datagrams are forwarded. JSON stderr inventory at readiness, every ten seconds
and shutdown includes global/per-mapping flow and traffic counts, quota/rate
rejections, oversize drops, expiry, socket errors and Linux `SO_RXQ_OVFL` drops.
The kernel delivers overflow counts on a subsequent received datagram, so final
unreported queue loss can remain unknown. Send failures (including ICMP errors
surfaced by connected sockets) are counted without retransmission. Logs omit
payloads and upstream/client addresses; config errors expose only a category.

Verify:

```sh
cargo test -p udp-relay --locked
cargo build -p udp-relay -p rtp-service --locked
unshare -Urnm bash udp-relay/tests/linux-rtp.sh
```

Socket tests require network syscall access (the Codex filesystem sandbox may
need escalation). Three real UDP fixtures cover two clients, two mappings, port
translation, zero-length/1200-byte exact payloads, source ports, interleaved and
delayed response demultiplexing, rebinding, oversized input/reply rejection,
foreign upstream rejection, flow/source quotas, rates, expiry, atomic startup
and restart.

The Linux fixture derives from `rtp-service/tests/linux-applications.sh`: five
disposable namespaces hold two clients, a relay, an endpoint and a separate
TCP/UDP backend. It provisions only explicit endpoint forwarding/return routing,
selects `initial_rotation_port:30001` with `rotation_seconds:0`, and opens only
base 30000 plus selected non-base 30001. Remaining rotation ports are bindable.
Both clients reach backend TCP/UDP, existing ping processes survive injected
same-port tuple repair and relay restart, and service shutdown removes its TUNs
and empties journals. The relay receives no key. This is local namespace evidence;
A5 adds coordinated dynamic/mTLS management and scoped public acceptance.

## Managed listener leases (A5)

`cpxy-udp-relay managed examples/managed.json` adds an optional management TCP
listener protected by mutual TLS. Use a dedicated management CA and separate
relay/server and controller/client certificates, with server DNS/IP SAN and
appropriate serverAuth/clientAuth usages. Every client certificate signed by
this dedicated CA has authority over the single configured owner; this version
does not multiplex differently authorized certificate identities. Restrict the
management listener with deployment firewall policy. VPN PSKs never enter this
crate. Its additional dependencies are rustls/PEM and the generic port-admission
crate; the RTP controller adapter lives in `rtp-service/src/relay.rs`.

[managed.json](examples/managed.json) is a complete relay configuration with
documentation addresses. Managed mode requires an empty static `mappings` list;
only the authority's configured listen/upstream IPs, explicit numeric port
allowlist and bounded flow limits can be requested. Generic requests may
translate between allowed ports. `agent_socket:null` explicitly chooses external
administrator admission; a configured socket uses the restricted local
firewalld agent and the matching owner/bind/pool policy. No fallback occurs on
agent failure. Provision the management TCP rule and host forwarding policies
separately. The relay itself requires no kernel forwarding or TUN.

The wire format is one JSON object/newline in each mTLS connection. Requests are
`{"operation":"inventory"}`, `{"operation":"prepare","spec":SPEC}`,
`{"operation":"commit","spec":SPEC,"boot":"READY_BOOT"}`,
`{"operation":"renew","spec":SPEC,"boot":"READY_BOOT"}` and
`{"operation":"retire","spec":SPEC}`. SPEC contains `owner`, `generation`,
`listen` and `upstream` (numeric socket addresses). The response contains `boot`,
`ready`, `entries` and nullable `error`. Every entry contains `spec`, `committed`,
`retiring` and Unix-second `expires`. READY requires an installed bound socket,
forwarding and verified admission. Commit/renew require the exact live generation
and current agent boot. Renew requires a committed entry. Repeated prepare does
not extend staged lifetime. A late staged commit fails; exact repeated retirement
is safe and cannot remove a replacement generation. Inventory is reconciled
before serving management on restart, with a fresh boot identity.

The journal is exclusively locked, atomically saved and fsynced before admission
mutations. Protect its directory/configuration with service identity permissions.
Live unexpired generations are rebound/restored after relay restart; flows are
recreated by incoming traffic. Staged preparations expire after `staged_seconds`
(2–120); committed leases require renewal within `outage_seconds` (10–86400).
Use 30/120 for the initial RTP deployment with at most four live ports. Both
controller and agent periodically reconcile; local/remote failure freezes new
RTP rotations while current/base/draining paths remain held. There is no atomic
two-host commit, and client validation/recovery remains necessary after READY.

TLS I/O has an absolute five-second deadline, a two-second connect timeout and a
64-KiB request/response cap. The serialized control thread performs bounded local
agent calls, so expiry/reconciliation can lag by one pending operation plus a
one-second poll; these are software deadlines, not native firewall timers.
Firewall reload may interrupt traffic until reconciliation restores admission.
Control work never executes in the packet poll: bounded channel commands only
install, disable or remove prepared descriptors. On retirement/expiry forwarding
is disabled before firewall release; failed release keeps the socket and journal
for retry, preventing numeric reuse. All expired leases are eventually retired
when the agent becomes available. A killed relay cannot execute software expiry:
use the independent `cpxy-udp-relay cleanup CONFIG` command through the supplied
[service template](deploy/cpxy-udp-relay@.service). It locks out live workers,
releases exact persisted generations via the local agent, and preserves failures
for retry. Stop relay/controller before the local agent/firewalld. If every
supervisor is lost, untimed admission persists until restart/explicit cleanup.

Verify network-enabled fixtures:

```sh
cargo test -p udp-relay -p rtp-service -p port-admission -p rtp-l3 --locked
cargo build -p udp-relay -p rtp-service -p port-admission --locked
python3 udp-relay/tests/management.py
unshare -Urnm bash udp-relay/tests/linux-coordinated.sh
```

The loopback fixture uses actual mTLS and opaque echo traffic: unauthorized TLS,
missing acknowledgement/retry, stale boot, conflict, late staged commit, occupied
socket, relay crash/restart, numeric reuse/stale retirement, committed controller
outage grace/expiry and independent cleanup. The namespace fixture uses two
clients, backend TCP/UDP applications, separate private firewalld/D-Bus/agent
instances on relay and endpoint, repeated dynamic rotation/numeric reuse, relay
firewall reload, management disconnect, same-port repair and forced relay death
with independent generation cleanup. Short UDP exchanges use bounded application
retry; no zero-loss rotation/soak guarantee is claimed. No worker OS sandbox or
public deployment is implied by these disposable local checks.

The scoped three-node public IPv4 acceptance also passed: one application process
completed 54 HTTP and 54 UDP exchanges over 120 seconds across rotation and a
forced relay restart, with two transient retries. Both public data hops were
captured, and original host state was restored after removing the temporary
deployment. See the [sanitized results](../docs/rtp-l3-a5-results.json) and
[A5 evidence](../docs/rtp-l3-application-plan.md#a5--coordinated-relay-rotation-and-firewall-lifecycle).
