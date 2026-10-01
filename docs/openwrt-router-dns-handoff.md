# OpenWrt router client: two DNS modes

Status: implementation proposal and agent handoff; no implementation in this change

Date: 2026-09-30

Target: one OpenWrt / GL.iNet router and firmware first, IPv4 TCP internet traffic

## 1. Requested outcome and decisions

Build an installable router package using the existing cpxy SOCKS5 client and a mature tun2socks implementation. The package configures the protected LAN's DNS, routing, firewall, and service lifecycle. Devices require no proxy settings. Keep general UDP forwarding out of the first prototype.

Implement both DNS strategies, selectable while the service is running:

1. `fake_ip`: query the uplink-provided resolver. Return local-region public addresses directly. For other results, return a persistent synthetic IPv4 address associated with the original hostname. A TCP connection to that address becomes a cpxy connection by hostname, resolved on the remote server.
2. `dual_resolve`: start local and remote DNS queries concurrently. A usable uplink answer containing only addresses in the local region wins immediately. Otherwise return the remote answer, waiting for it if necessary. TCP connections use the returned real IP; local-region public IPs connect directly, other public IPs connect through cpxy.

Terminology: **local region** means the region represented by the configured direct-routing GeoIP dataset; **non-local region** means public addresses outside that set. Reuse the repository's existing embedded dataset for the prototype. These terms describe routing policy and do not imply a new dataset or a change in classification behavior. Public-facing descriptions, configuration help, status messages, and tests should use this neutral terminology.

Use `fake_ip` as the initial default to exercise Solution 1; both modes are mandatory deliverables. Do not replace Solution 2 with a sequential local-then-remote lookup. Do not add general UDP commands or rewrite the TCP/IP stack.

The user has requested a plan, not deployment. The next implementer should follow this document, record justified deviations, and ask for the router model/firmware only when needed for device packaging and validation. Linux namespace integration tests and core implementation can proceed before that information is available.

### Privacy and trust boundaries

This is a **region-based split routing policy**. Destinations in the local region intentionally see the router's ISP public IP. It supersedes the earlier all-destinations-through-VPN proposal for this prototype. It cannot meet the stronger requirement that every public-IP lookup, including one hosted in the local region, always shows the remote exit IP.

Both modes disclose queried names to the uplink DNS resolver. Solution 2 also submits queries to the remote resolver even when the local result ultimately wins; cancellation cannot undo an already sent query.

The observation that incorrect DNS answers are non-local-region addresses is a deployment assumption supplied by the user, not an authenticated signal. Untrusted answers containing local-region addresses can pass the direct test. GeoIP says where an address is classified, not whether a DNS answer is genuine. The remote resolver is assumed to operate outside the affected DNS environment; tunneling a query alone does not establish DNSSEC authenticity.

## 2. Existing code and reuse boundaries

| File | Current behavior / intended use |
|---|---|
| `client/src/bin/standalone.rs` | Local HTTP/SOCKS5 listeners using `ProtocolOutbound`; useful starting point. |
| `client/src/socks_proxy_server.rs` | SOCKS5 CONNECT only; accepts IPv4, IPv6, and domain targets. Add real SOCKS5 failure replies for router errors. |
| `client/src/outbound/protocol.rs` | One upstream TCP connection per destination, optional outer TLS, encrypted HTTP handshake, WebSocket stream. Reuse for data connections. |
| `cpxy-ng/src/protocol.rs` | Unversioned rkyv request with host, port, TLS and cipher settings. Do not change its serialized layout in place. |
| `cpxy-ng/src/http_protocol.rs` | Encodes requests in HTTP path/header and response metadata in a header. Refactor transport helpers to support a separate versioned request. |
| `server/src/server.rs` | TCP destination connection; hostname resolution occurs through `TcpStream::connect`. Needs a separate resolver command handler. |
| `client/src/outbound/direct.rs` | Can connect to an explicitly supplied IPv4 address. Reuse for direct TCP without resolving the hostname again. |
| `client/src/outbound/` (existing regional policy module) | Existing regional/site/Tailscale policy and UDP resolver. Do not use unchanged: it defaults unresolved destinations to direct and has unrelated routing exceptions. |
| `client/src/outbound/resolving_ip.rs` | Takes only the first IPv4 result; insufficient for whole-answer classification. |
| `cpxy-ng/src/geoip.rs`, `geoip-data` | Reuse embedded local-region IPv4 lookup; an address absent from this local-region-only database is not necessarily confirmed non-local-region. |
| `geoip-data/SOURCE.md` | Data provenance/update procedure; record dataset identity in runtime status and tests. |

Preferred structure: a new `router` workspace crate with binary `cpxy-router`, depending on `client`, `cpxy-ng`, and `geoip-data`. Keep DNS/policy/storage code independently testable. Existing Android and standalone entry points retain their behavior.

Suggested modules: `config`, `control`, `dns/{server,local,remote,policy,cache}`, `fake_ip_store`, `outbound`, `status`. Shared protocol additions belong in `cpxy-ng`; remote resolution belongs in `server`. Package/service scripts belong under `packaging/openwrt/`.

## 3. Data path and ownership

```text
Protected device DNS (UDP or TCP port 53)
  -> router dnsmasq frontend (local DHCP names only; no internet-answer cache)
  -> cpxy-router DNS policy listener on loopback
       -> local resolver supplied by the active uplink, directly over WAN
       -> remote Resolve command, when the mode/policy needs it

Protected device internet TCP
  -> separate LAN policy routing table -> TUN -> tun2socks
  -> cpxy-router SOCKS5 listener on loopback
       -> fake IP: SQLite lookup -> ProtocolOutbound(original hostname)
       -> real local-region public IP: DirectOutbound(exact IP)
       -> other public IP: ProtocolOutbound(exact IP)

Router-originated connections, including both outbound branches
  -> ordinary routing table -> WAN
```

For prototype simplicity, capture all protected internet TCP before deciding direct versus proxy in Rust. A direct connection is still locally relayed through tun2socks/SOCKS5, but never visits the remote cpxy server. This satisfies the network-path meaning of "direct" and avoids duplicating regional routing decisions in kernel IP sets. Kernel-level direct forwarding for local-region flows is a later optimization.

DNS UDP on the LAN and router-to-resolver UDP are allowed. "No UDP protocol" means no general client UDP relay through cpxy. Reject other protected internet UDP, including QUIC, even for local-region destinations. Allow essential local DHCP and explicitly configured local services. Internet ICMP is not forwarded by this prototype; preserve ICMP needed by the router's own networking and path-MTU handling.

Do not globally replace the router's default route. Select the TUN routing table by the protected ingress interface or a mark applied only to protected forwarded traffic. Router output must not inherit that mark. Handle router-local addresses and configured local-network exceptions before capture. Keep a terminal unreachable route/rule and forwarding deny policy so a missing TUN route cannot fall through to WAN.

## 4. Shared address and DNS policy

### 4.1 Classification

Define one shared classifier, in this order:

1. Configured fake pool -> `Fake` (always process before special-address checks).
2. Explicitly permitted LAN/service destination -> `LocalAllowed`.
3. Loopback, link-local, multicast, unspecified, broadcast, documentation, benchmarking outside our pool, and other prohibited/non-global ranges -> `RejectedSpecial`.
4. Public IPv4 present in the embedded local-region set -> `LocalRegionPublic`.
5. Other public IPv4 -> `ProxyPublic`.

Use a maintained special-address table or equivalent explicit logic; `!is_private()` is not sufficient. Local exceptions must be narrow; do not automatically permit all RFC1918, all CGNAT, or the existing code's entire `100.0.0.0/8` range. An uplink gateway is not automatically a permitted destination for protected devices.

For a public A question, a **local direct-eligible answer** must have:

- A valid matching DNS response with NOERROR and a complete, bounded CNAME chain ending in one or more A records.
- Every relevant terminal A address classified as `LocalRegionPublic`.
- No conflicting, malformed, incomplete, truncated, or looping answer chain.

Classify the complete relevant RRset, never just its first address. Ignore unrelated additional records for region classification and do not expose unvalidated address-bearing extras. Mixed local-region/non-local-region answers take the proxy/remote branch as a whole. Timeouts, NXDOMAIN, NODATA, SERVFAIL, invalid addresses, and unknown classification do not authorize direct access.

Local DHCP names and explicitly configured private zones are handled by dnsmasq before this public-DNS policy. A private address returned for an arbitrary public hostname is not evidence of a local service.

### 4.2 DNS behavior common to both modes

- Listen for both UDP and TCP DNS, IN class, one question per request. Use an established DNS parser; validate response source, ID, question, opcode, and lengths. Retry truncated upstream UDP responses over TCP. Bound chain depth, message sizes, concurrency, and deadlines.
- Rebuild replies with the original client transaction ID and question. Do not replay raw cached IDs or stale TTLs. Support EDNS sizing and correctly set TC when a UDP answer does not fit; TCP provides the complete answer.
- `A`: use the selected strategy below.
- `AAAA`: return policy NOERROR/NODATA with no address; also disable public IPv6 advertisement and block protected IPv6 internet forwarding. DNS suppression alone is insufficient.
- `HTTPS` / `SVCB`: return policy NOERROR/NODATA in v1, avoiding address hints that circumvent fake-IP handling. This deliberately sacrifices those records' optimizations/features in both modes. Do not pass through `ipv4hint`/`ipv6hint` unchecked.
- Ordinary non-address queries (for example TXT/MX/SRV/PTR outside the fake pool): resolve remotely through the new command, preserve relevant records and TTLs, and strip unrelated additional address records. Local zones remain local. Reject AXFR/IXFR, UPDATE, and unsupported operations rather than offering a general DNS tunnel.
- PTR queries inside the fake pool can return the recorded hostname for debugging; an unallocated address returns no data. Never forward fake-pool reverse lookups to WAN.
- Clear AD on policy-modified responses; do not claim DNSSEC validation. Fake answers and policy NODATA cannot satisfy independent DNSSEC validation. Full DNSSEC-validating client compatibility is outside this prototype.
- Cache entries are scoped by mode, configuration generation, resolver identity, qname/qtype/qclass and any supported answer-affecting flags. Disable ECS in requests; do not forward client subnet metadata. Avoid raising TTLs. Use library support for negative caching and resolution-failure caching; do not confuse NXDOMAIN with transport errors.

Applications using DoH over HTTPS can resolve names outside this DNS policy. Redirect ordinary port 53 to the router and block direct DoT port 853 as a prototype policy, but do not claim this catches arbitrary DoH. Real IP traffic still follows the regional direct/proxy classifier; resolving names outside our DNS service can cause untrusted non-local-region IPs to be proxied by IP and fail. Document this limitation rather than attempting TLS interception.

## 5. Solution 1: `fake_ip`

### A-query algorithm

1. Snapshot the active configuration generation and normalize the qname (case-insensitive DNS identity, consistent trailing-dot handling; retain the original question for replies).
2. Serve a valid generation-specific cache entry if present; otherwise query the active uplink resolver set under the local deadline.
3. If the answer is direct-eligible, return its validated A/CNAME chain, with TTLs capped by configuration. Cache the result as local/direct.
4. For a valid non-local-region/mixed answer, discard all its IP addresses. Allocate or find one fake IPv4 address for the **original queried hostname**, commit it durably, and return a single synthetic A record for that qname. Do not preserve an untrusted CNAME chain in this synthetic answer.
5. For local negative/error/timeout or unusable answers, also synthesize by default. This follows the principle that an untrusted local failure must not prevent a server-side attempt. Track the reason separately. It can make nonexistent names appear to resolve; the eventual remote TCP connect then fails. This is an explicit prototype tradeoff. Do not convert this into direct-on-error.
6. Use a configurable synthetic TTL, initially 60 seconds. This TTL controls answer caching, not ownership of the fake address.

For a TCP connection to an allocated fake address, look up its original hostname and send the existing cpxy CONNECT request with that hostname and the requested destination port. Set destination `tls=false`: the application supplies its own TLS bytes. Keep verified TLS enabled on the router-to-cpxy transport. Do not resolve this hostname locally again, inspect SNI, or replace it with the untrusted address.

For a missing mapping inside the fake pool, fail the connection and increment a diagnostic counter. Never connect directly or proxy the fake IP as a literal. Existing mappings must work regardless of the currently selected DNS mode.

Most ordinary HTTPS applications retain the hostname for SNI and certificate verification even when DNS returns a synthetic IP. Compatibility is nevertheless an experiment: special-IP filters, DNSSEC validation, IP-bound protocols, software exposing IPs to another peer, and cached-address behavior may fail. Solution 2 provides the comparison path.

### SQLite contract

Use SQLite through **SQLx's async SQLite support with Tokio** as the durable ownership registry, with an in-memory lookup cache. A DNS cache and an address-ownership registry have different lifetimes.

Suggested schema (integer addresses use consistent unsigned IPv4 values stored in SQLite INTEGER):

```sql
CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE fake_ip_mapping (
    fake_ipv4 INTEGER PRIMARY KEY,
    hostname TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    last_answered_at INTEGER NOT NULL,
    last_connected_at INTEGER,
    allocation_generation INTEGER NOT NULL
);
```

Store pool CIDR, allocator position, and dataset/config diagnostics in metadata. SQLx's migration history is the source of truth for schema versions; do not maintain a second schema-version counter in application metadata. Do not store credentials in this database. Use async SQLx transactions and database uniqueness constraints to make concurrent first queries allocate one stable mapping.

Use a shared `SqlitePool` with a small, explicitly bounded connection count (initially one), bounded acquisition/busy timeouts, and parameterized queries. SQLx manages SQLite's blocking work internally; application database calls should use its async API rather than a separate hand-written synchronous database worker. Configure `SqliteConnectOptions` explicitly: foreign keys enabled, WAL journal mode, and FULL synchronous durability for allocations. Verify those settings on the target filesystem, bound/checkpoint WAL growth, and close the pool gracefully. Enable database creation only during explicit first initialization; ordinary startup must not recreate a missing established database. Select and pin a SQLx release compatible with the workspace toolchain and OpenWrt target, enabling only the needed SQLite, Tokio runtime, migration and macro features.

### Schema migrations

- Commit ordered, versioned SQL migrations under `router/migrations/`, beginning with `0001_initial.sql` for the schema above and its constraints/indexes. Every later schema change is a new migration; never edit, reorder or remove a migration that has shipped. Keep SQL files using LF line endings for stable checksums.
- Embed the migrations in the binary with `sqlx::migrate!("./migrations")` and run the migrator asynchronously against the pool. Add `router/build.rs` with `cargo:rerun-if-changed=migrations` so adding a migration rebuilds the embedded set. Production routers need neither the SQLx CLI nor a separate migrations directory.
- Run and validate migrations during startup, before serving DNS/SOCKS5 requests or reporting readiness. Keep forwarding protection active throughout. Only one service instance may own and migrate the database; acquire an instance lock before opening it for migration. Use SQLx migration transactions for the supported SQLite DDL and do not opt out of transactions in the prototype.
- Treat migration errors, checksum mismatches, dirty/incomplete history, and a database containing versions newer than the binary knows as startup failures. Do not ignore missing migrations, reset migration history, auto-downgrade, or delete/recreate the database. Report the failing version without exposing mapping contents.
- Upgrades must preserve every fake-IP-to-hostname association and allocator state. Before migrating an existing installation, create a consistent backup using SQLite's backup facility or a quiesced, checkpointed database; copying only the main file while WAL writes are active is insufficient. Define restoration as an explicit operator action with the service stopped.
- A runtime DNS-mode switch does not run migrations or rebuild tables. Package upgrades restart through the same migration entry point; rerunning with an up-to-date schema is a no-op. Prefer forward migrations; document binary downgrade compatibility rather than automatically applying destructive down migrations.
- Keep database schema tests independent of a live development database. Runtime parameterized SQLx queries are sufficient; if compile-time query macros are used, commit and check the required offline metadata so clean/cross builds do not depend on a developer's database.

### Allocation and persistence rules

- Suggested pool: `198.18.0.0/15`, configurable; it is special benchmarking space, not public address space. Verify no overlap with LAN/WAN/VPN routes or the TUN interface's own address subnet. Do not use the tun2socks example's interface address if it conflicts with this allocation pool.
- Allocate monotonically and **do not reassign an address to another hostname in the prototype**, even after DNS TTL expiration. Endpoint caches can outlive TTLs, and reassigning an address could connect a device to the wrong hostname.
- Commit a new mapping before emitting its DNS answer. Use durable storage on the router overlay or configured persistent volume, with verified SQLite durability settings. On commit failure, return SERVFAIL. Persisting only in `/tmp` is not sufficient.
- Retain mappings on mode switch, service restart, and normal package upgrade. Pool changes require an explicit migration/reconfiguration operation and are not ordinary live settings.
- Never silently delete/recreate a corrupt or unexpectedly missing established database. Block fake-pool connections and fail startup/activation with an actionable error. First installation initializes a new registry explicitly.
- Pool exhaustion returns SERVFAIL for new allocations; existing mappings continue. Expose occupancy. Database reset must warn that clients may still hold old addresses and cannot be made safe merely by waiting one TTL.
- Batch last-used timestamp writes to limit flash wear. Allocation writes remain synchronous/durable. Keep a bounded memory cache; cap concurrent new allocations. DNS positive/negative cache may remain in memory; persistent DNS cache is not required.

## 6. Solution 2: `dual_resolve`

Start both branches immediately on a cache miss:

```text
L = local resolver query using the uplink DNS servers
R = authenticated cpxy Resolve(qname, qtype) to the remote server

L direct-eligible -> return L immediately; cancel/ignore R
L otherwise      -> use R when available
R arrives first  -> retain it until L completes or the local deadline expires
L times out      -> use R when available
R fails          -> still allow L to produce a direct-eligible answer
neither usable   -> SERVFAIL, never fall back to an untrusted uplink answer
```

A remote answer is not automatically the winner because it arrives first. Local eligibility has priority until the local deadline. Proposed initial tunables: local deadline 800 ms; remote branch deadline 5 s from launch; overall DNS deadline 6 s. Tune from measurements; do not serialize deadlines or let discarded tasks run indefinitely. Limit concurrent queries and coalesce identical in-flight work. Cancel a shared branch only when it has no remaining waiters.

| Local outcome | Remote outcome | Client answer |
|---|---|---|
| All terminal A addresses public and in the local region | Anything, including failure | Local answer immediately |
| Outside the local region, or mixed regions | Valid positive response | Validated remote response |
| Negative/error/timeout/unusable | Valid positive response | Validated remote response |
| Not direct-eligible | NXDOMAIN or NODATA | Remote negative response |
| Not direct-eligible | Timeout/transport error/SERVFAIL | SERVFAIL |
| Still pending | Remote arrives first | Hold remote result until local decision/deadline |

Validate remote answers structurally too; reject special-use addresses for public names (fail the response as unusable rather than partially rewriting a signed/mixed RRset). Return the original remote CNAME/A chain and remaining TTLs. The remote response is authoritative for the selection policy but is a recursive resolver result, not necessarily an authoritative-server answer.

TCP routing is based on the returned **actual destination IP**, without a hostname reverse map. If a remote lookup returns a local-region public IP, that connection is direct under the user's requested policy. If the intended future rule becomes "every remote-selected answer must be proxied even when its IP is in the local region," that is a different policy requiring additional provenance/IP tracking; do not silently implement it here.

For proxied real IPs, construct `ProtocolOutbound` with the IP literal as `host`; do not populate a domain field with an old hostname because `ProtocolOutbound` uses `host.host()`. Preserve the application's bytes, including TLS SNI, and set destination `tls=false`. The server connects to the supplied IP without another DNS lookup.

## 7. Versioned remote DNS command

### Scope and compatibility

Add an authenticated `Resolve` operation to our protocol. It performs DNS on the remote server and returns DNS results; it is not a general UDP association or a client-selected arbitrary UDP destination.

The existing rkyv request has no version tag. Preserve legacy request/response types and wire layout. Introduce distinct v2 envelopes with an explicit magic/version **inside authenticated plaintext** before the new serialized payload. Decrypt once, dispatch by the authenticated prefix, and otherwise decode legacy v1. An outer HTTP version hint may help parsing but must not be trusted without matching the authenticated envelope. Never rely on "try deserializing both layouts" as negotiation.

Keep existing CONNECT data traffic using v1 for this milestone. v2 initially needs `Resolve` and `Capabilities` only. New servers accept old clients. Old servers may reject v2; detect that during capability checks and report it clearly. Switching to `dual_resolve` must fail validation if remote resolution is unavailable; never silently fall back to untrusted uplink answers outside the local region. Both complete DNS modes require a server with Resolve support for the non-A query policy.

### Suggested exchange

1. Establish the existing transport with verified outer TLS, normally terminated by the existing deployment's TLS reverse proxy (the current Rust server accepts plaintext TCP). Keep PSK authentication.
2. Send v2 request envelope in the established HTTP handshake carrier. Include command, bounded qname/qtype/qclass and supported DNS flags for Resolve, fresh request ID, timestamp, and full stream cipher settings. Do not overload `host` or use a magic destination port to signal DNS.
3. Receive a small authenticated v2 response (`Ready` / typed error; request ID must match), upgrade to the existing WebSocket stream, and transfer one bounded result frame.
4. For Resolve, frame format: 32-bit network-order length followed by one DNS wire-format response, maximum 65,535 bytes. DNS wire data preserves CNAMEs, RCODE, TTLs and negative SOA details. This is separate from the two-byte length used for DNS-over-TCP on port 53.
5. For Capabilities, use a separately defined bounded capability payload listing protocol version and supported operations. Close after one operation in v1 of this feature; connection pooling/multiplexing can follow measurements.

Keep large DNS responses out of HTTP headers and `initial_response`. Validate lengths before allocation. Define deterministic error codes for unsupported command/type, invalid query, resolver timeout, resource limit, and internal failure. DNS NXDOMAIN is a valid DNS result, not a transport error.

Use existing maintained Hickory components where suitable (already a client dependency). Add a server resolver abstraction returning complete DNS semantics, not just `getaddrinfo` addresses. Server upstream configuration is controlled by the server administrator, supports UDP with TCP fallback or TCP, and must not inherit the router's untrusted resolver. Restrict operations and rate/concurrency limits; bind the endpoint like the current authenticated service, not an open public recursive DNS listener.

The Resolve request must not make the current TCP destination connection or wait through the existing 500 ms initial-response read. It dispatches directly to the resolver handler. Requests have cancellation/deadlines and no unbounded detached tasks. Full stream encryption plus verified outer TLS are mandatory in the router profile; do not use port-based partial encryption for DNS control messages.

Specify the exact envelope schemas, timestamp tolerance, request-ID handling and frame fixtures in tests during implementation. Ensure authentication covers version, command and parameters. Treat unknown versions as errors. A capability probe should be a real protocol operation, not a query to an arbitrary public website.

## 8. Runtime mode switching and caches

Expose a root-owned local control socket and CLI, for example:

```text
cpxy-router status
cpxy-router dns-mode set fake_ip
cpxy-router dns-mode set dual_resolve
cpxy-router mappings list --limit 50
cpxy-router dns-cache flush
```

These are proposed commands to implement, not currently available commands. Persist settings through the package's configuration owner (UCI on OpenWrt); avoid independent UCI and JSON sources of truth. CLI/config reload both call the same validation and activation path.

Switch procedure:

1. Validate the new configuration and required remote capabilities. A failure leaves the previous mode active.
2. Persist the accepted setting and atomically publish a new immutable configuration generation. Report success only once persistence and activation agree; roll back on failure.
3. New DNS requests use the new generation. In-flight requests finish using their captured generation; their results cannot populate the new cache namespace.
4. Clear/invalidate application DNS answer caches. Configure dnsmasq without an internet-answer cache from the start, so there is no hidden frontend cache to flush. Preserve DHCP/local-zone handling.
5. Keep the TUN, firewall protection, fake-pool route, SQLite mappings and active TCP streams alive. Existing TCP streams keep their selected outbound path.

`fake_ip -> dual_resolve`: clients may retain fake addresses; those still resolve through SQLite and connect by hostname. `dual_resolve -> fake_ip`: clients may retain real addresses; those still use the shared real-IP classifier. Do not reject either form merely because the current DNS mode differs.

The switch affects future uncached DNS decisions. The router cannot flush browser/OS caches remotely or instantly move established connections. State this in CLI/status output and tests. No forced disconnection by default.

Uplink DNS changes increment a resolver generation and invalidate local-answer caches; server/resolver changes invalidate remote caches. GeoIP updates invalidate classification-dependent caches. Fake mappings survive all of these events.

## 9. OpenWrt packaging and DNS integration

Deliver one `cpxy-router` package with explicit dependencies on the selected pinned tun2socks implementation, TUN support, and SQLite support as built. Choose target-compatible versions after obtaining model, architecture, firmware, available storage/RAM, and firewall backend. Do not assume every GL.iNet device runs firewall4/nftables or has the same package format/feed.

- Use procd for process supervision, service reloads and network-change triggers. A netifd interface handler can own the TUN lifecycle; keep ownership unambiguous between netifd and service scripts.
- Package provides one configuration section: upstream cpxy endpoint/secret reference, protected network, DNS mode, deadlines, fake pool/database path, local-zone exceptions and DNS overrides. Bind SOCKS5 and internal DNS to loopback; secrets must not appear in logs or status output.
- Preserve dnsmasq DHCP and local names. For the protected network, forward public DNS to the cpxy policy listener with no alternate upstream fallback and disable its public-answer caching. Redirect protected clients' UDP/TCP port 53 to that frontend. If sharing dnsmasq with other networks prevents isolation, create a dedicated protected-network instance and document port/interface ownership.
- Obtain uplink-provided resolver addresses from netifd's active uplink state or a platform adapter. Do not discover upstream DNS by following `/etc/resolv.conf` after changing dnsmasq forwarding: that can point back to ourselves. Detect resolver loops. Support explicit configured uplink DNS IPs as a fallback, never a silent public resolver substitution.
- Router system DNS and cpxy endpoint bootstrap resolution stay on a separate explicit WAN resolver path. Prefer a configured bootstrap IP with the original TLS server name retained when uplink DNS returns an incorrect address for the cpxy endpoint. Track uplink changes and permit normal router output without tunneling loops.
- Install protection before enabling protected forwarding. Block raw protected LAN-to-WAN internet forwarding even for local-region destinations; allowed direct TCP is opened by the router client after classification. Permit LAN-to-TUN and return traffic. Keep protection active on worker failure and transient reloads.
- Disable conflicting flow/hardware offload for the protected path in the prototype; handle preexisting direct connections when enabling protection. Ensure established/related or cached flow rules cannot preserve an unintended direct path. Scope any conntrack cleanup to the protected network.
- Block protected IPv6 internet access and disable public IPv6 prefix advertisement there. Preserve local IPv6 control traffic needed for the LAN rather than indiscriminately dropping all ICMPv6.
- Keep fake-pool destinations confined to local interception. They must never reach WAN, even after service failure or mode changes.
- Startup/reload is idempotent and owns named rules/tables/sections. Never flush unrelated firewall rules. WAN reconnect and firewall reload reapply protection without an interval of unintended direct forwarding. Reject unsupported conflicting VPN/PBR setups with diagnostics for this first target.
- Distinguish `stop` (retain protection; internet TCP unavailable) from explicit `disable` (restore the prior routing/DNS policy and allow ordinary internet access). Preserve enough owned-state metadata to uninstall cleanly. Show the impact of disable explicitly.

A GL.iNet VPN dashboard entry or LuCI page is optional after the CLI/service path works. The initial release must not depend on undocumented dashboard internals.

## 10. Observability and proposed defaults

Expose mode/config generation, active uplink DNS, remote Resolve capability, fake-pool occupancy, DB health/path, GeoIP dataset identity, TUN/service state, and last setup error. Redact credentials and default to aggregated counters rather than per-domain persistent logs.

Counters: local-region wins, uplink results outside the local region or spanning regions, local errors/timeouts, remote wins/errors/timeouts, canceled remote work, cache hits per mode, fake allocations/hits/misses, direct/proxy TCP counts, blocked special addresses, blocked UDP/IPv6, and mode switches. Record latency histograms for both branches and the final DNS response; these are needed to compare the two designs.

Initial defaults to tune on hardware:

| Setting | Default |
|---|---|
| DNS mode | `fake_ip` |
| Local deadline | 800 ms total per query, including retries |
| Remote branch deadline / overall deadline | 5 s / 6 s |
| Fake A TTL | 60 s |
| Real positive TTL cap | 300 s, never above upstream remaining TTL |
| Local/remote concurrent query limits | 128 / 64; bounded overload errors |
| CNAME depth | 16, with explicit loop detection |
| Fake address allocation | Stable, durable, never reused automatically |
| DNS cache | Bounded in memory, generation-scoped |
| IPv6 / general UDP forwarding | Disabled for protected internet traffic |
| Router-to-server TLS | Required with hostname verification |

## 11. Implementation milestones

1. **Policy and fixtures.** Add the router crate, pure classifiers and DNS decision types. Record current v1 wire fixtures before refactoring. Build fake local/remote resolvers with controlled delays and deterministic answers.
2. **Remote Resolve.** Add explicit version dispatch, capabilities, resolver handler and bounded result framing. Prove old client/new server compatibility and clear old-server rejection. Reuse transport helpers without changing legacy wire semantics.
3. **DNS modes and storage.** Implement UDP/TCP DNS handling, whole-answer policy, both strategies, coalescing/cache limits and async SQLx mapping allocation. Add embedded versioned schema migrations, startup validation and consistent upgrade backups. Confirm durable-before-answer behavior.
4. **TCP integration.** Add router-specific outbound selection and actual SOCKS5 error replies. Test fake hostname forwarding, exact-IP forwarding and direct TCP. Drain completed tasks in listener supervision; the current `JoinSet` must not grow forever without reaping finished connections.
5. **Live control.** Implement atomic configuration generations, persistent mode changes, status and mapping inspection. Test old fake/real cached destinations across switches.
6. **Linux integration.** Run a protected client namespace, router namespace, controlled local resolver, tun2socks and remote cpxy server. Verify routing and packet captures without requiring a physical router.
7. **One OpenWrt target.** Package for the actual model/firmware; install service, DNS integration and firewall lifecycle. Document concrete installation, upgrade, stop/disable and recovery steps.
8. **Hardware comparison.** Exercise both modes on the same network and domains, measure DNS latency and throughput/CPU/RAM/storage writes, and record app compatibility. Do not advertise a leak-free result based only on successful browsing.

## 12. Required acceptance tests

### DNS and policy

- Uplink A answer containing only addresses in the local region in both modes: real IP returned and direct connection to exactly that IP.
- Untrusted non-local-region answer: fake mapping/hostname CONNECT in Solution 1; remote real IP/IP CONNECT in Solution 2. Assert the untrusted literal never becomes the TCP destination in either successful path.
- Mixed regional RRsets in both record orders; multiple CNAMEs; incomplete chains; CNAME loop; empty NOERROR; NXDOMAIN; SERVFAIL; malformed response; truncated UDP with TCP retry.
- Remote-first, local-first, both timeouts, remote error followed by local-region success, and local-region success with a remote task that must be canceled. Use controlled clocks/delays, not timing-sensitive internet tests.
- Remote response with a local-region IP, mixed real addresses, or special-use IPs: verify the explicit rules above.
- Matching-ID/question/source validation, EDNS limits, TCP framing, DNSSEC AD handling, negative TTL decrement, HTTPS/SVCB/AAAA policy and unsupported operation rejection.
- DNS over UDP/TCP from devices works although general UDP relay is absent. Explicit local names continue to work.

### Mapping and runtime control

- Initialize a fresh on-disk database using the embedded SQLx migrations; rerunning startup is a no-op. Verify tables, indexes, constraints and migration history.
- Upgrade fixtures from every supported prior schema with existing mappings; verify exact address/hostname associations and allocator state survive. Test migration rollback on failure, interruption/restart, checksum mismatch, newer-schema rejection, instance-lock contention, and backup restoration with WAL enabled.
- Verify async SQLx pool exhaustion/busy timeouts fail within bounds without blocking unrelated DNS tasks. Confirm database creation is restricted to initialization and a mode switch never changes migration history.
- Parallel allocations of case variants yield one canonical hostname mapping; different original qnames sharing a CNAME may have distinct stable mappings.
- Allocation transaction failure emits no fake answer; restart preserves previously answered mappings. Missing/corrupt established DB fails safely.
- TTL expiry does not permit reassignment; pool exhaustion does not corrupt old entries. Unknown fake IP fails without an outbound connect.
- Switch both directions under active traffic; old fake and real addresses remain usable according to their rules. In-flight old-generation DNS results never contaminate the new cache.
- Failed capability/config validation leaves the old mode active. Persisted mode survives restart. No mode switch recreates TUN or opens a forwarding gap.

### Protocol and transport

- Legacy v1 fixtures and existing `full_tunnel_echo` pass. New Resolve responses retain CNAME, TTL and negative-answer semantics; oversized/malformed/unauthenticated frames fail before unbounded allocation.
- Old client/new server works; new Resolve/old server fails explicitly. Incorrect PSK, unknown version, expired requests and request-ID mismatches are rejected.
- For transparent HTTPS, application TLS reaches the destination unchanged; destination `tls=false` avoids double TLS. Test both hostname and literal-IP cpxy CONNECT.

### Router lifecycle and traffic capture

- Protect selected LAN only. Router-originated traffic and cpxy transport use WAN normally with no loop. Router administration/DHCP remain accessible.
- Capture WAN: expected direct local-region TCP, local DNS and cpxy endpoint traffic are allowed; no raw protected non-local-region TCP, fake-pool packets, general client UDP or protected global IPv6 escapes. Router background traffic is identified separately in assertions.
- Kill cpxy, tun2socks and DNS workers independently; restart/reboot/reload firewall/change uplink. Verify no direct fallback, and that preexisting offloaded/conntracked connections cannot continue outside the active routing policy.
- Explicitly test limitations caused by browser-managed DoH, QUIC fallback and a UDP-only application's expected failure. Test clients with pre-cached real addresses.
- Verify DNS mode switching, service stop versus disable, configuration rollback, and clean uninstall on the target firmware.

Deliver test commands/results and known limitations with the implementation. Hardware-dependent gates remain visibly pending until exercised on the actual router.

## 13. References

These references support platform/DNS constraints; the two policies and suggested defaults above are this project's design decisions.

- [tun2socks Linux setup and local-proxy routing-loop caveat](https://github.com/xjasonlyu/tun2socks/wiki/Examples).
- [OpenWrt custom network protocol handlers](https://openwrt.org/docs/guide-developer/network-scripting) and [procd service integration](https://openwrt.org/docs/guide-developer/procd-init-scripts).
- [GL.iNet VPN dashboard](https://docs.gl-inet.com/router/en/4/interface_guide/vpn_dashboard/): do not assume a generic SOCKS5 service receives its routing/kill-switch integration automatically.
- [IANA IPv4 special-purpose registry](https://www.iana.org/assignments/iana-ipv4-special-registry/): identifies `198.18.0.0/15` as benchmarking space, not globally reachable.
- [RFC 7766](https://www.rfc-editor.org/rfc/rfc7766.html): DNS-over-TCP support and transport behavior.
- [RFC 5452](https://www.rfc-editor.org/rfc/rfc5452.html): DNS response-matching and forgery-resistance requirements; these do not authenticate an untrusted on-path resolver.
- [RFC 2308](https://www.rfc-editor.org/info/rfc2308/) and [RFC 9520](https://www.rfc-editor.org/info/rfc9520/): negative answers and resolution-failure caching.
- [RFC 9460](https://www.rfc-editor.org/rfc/rfc9460.html): HTTPS/SVCB records can contain address hints that require explicit policy.

- [SQLx embedded migrations](https://docs.rs/sqlx/latest/sqlx/macro.migrate.html), [Migrator](https://docs.rs/sqlx/latest/sqlx/migrate/struct.Migrator.html), and [SQLite connection options](https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteConnectOptions.html): embedding, startup migration validation, rebuild tracking, and explicit connection settings.
