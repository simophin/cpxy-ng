# Standalone UDP relay

`cargo build -p udp-relay --locked` produces `target/debug/cpxy-udp-relay`.
Run `cpxy-udp-relay run CONFIG.json` as a local administrator/service identity.
Configuration is trusted administrator input, protected through local filesystem
and service-manager permissions. There is no network management listener in this
static delivery. Packet senders cannot choose upstreams. The crate has only
serde/JSON/libc dependencies; it does not load RTP, TUN, VPN keys or payload codecs.

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
public deployment and coordinated dynamic/mTLS management are A5 work.
