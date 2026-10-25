# RTP L3 application and relay plan

Status: proposed implementation plan, 2026-10-25. This replaces the completed
[PoC plan](rtp-l3-poc-handoff.md). The validated baseline is `451a27c`;
[Z4 acceptance](rtp-l3-z4-repair-results.md) is closed. Further ISP/soak campaigns
are not prerequisites for this phase. Checks below exercise the new features.
A1 now provides workspace core/service code and isolated Linux verification.
No host deployment is implied by the checked milestones.

## First useful application

A configured Linux client reaches backend services through a generic UDP relay.
The endpoint terminates the RTP tunnel and owns the TUN/routes. The relay forwards
opaque datagrams and has no VPN PSK, decrypted STATE or inner IP traffic. Both
measured data hops use public IPv4 endpoints; SSH/overlay management is separate. Actual hosts, addresses and credentials stay in private
deployment configuration.

```mermaid
flowchart LR
    C[Client apps and TUN] <-->|RTP UDP: public entry| R[Generic UDP relay]
    R <-->|Opaque RTP UDP: public backend| E[RTP endpoint and TUN]
    E <-->|Explicit routes and return path| S[Backend services]
```

The relay is a standalone tool for arbitrary UDP services; RTP-specific port
coordination is an adapter owned by the RTP service controller. The first RTP
profile selects one endpoint. Multiple RTP endpoints use separate named profiles
with disjoint relay listener/port namespaces. Do not distribute packets from one client tunnel across backends.
An endpoint switch establishes a new tunnel session. Reaching several servers
behind one RTP endpoint uses L3 routes, not packet inspection at the relay.

## Decisions for the first implementation

- Preserve the [current protocol](rtp-l3-protocol.md), validated candidate paths,
  same-port repair and established experimental security choices. A relay does
  not require another RTP format, crypto termination, or UDP-over-TCP wrapper.
- Ship a managed Linux service and a narrow CLI/profile API first. GUI/mobile
  adapters follow the working direct and relayed service paths.
- Make UDP port ownership a service abstraction. Binding, firewall admission,
  relay readiness and advertisement become one lifecycle operation.
- Use firewalld runtime operations through a privileged local agent. Do not
  expose broad firewall authority to the packet worker or use a subprocess per
  datagram. See [firewalld design](rtp-l3-firewalld-design.md).
- Implement a generic user-space UDP relay with per-client-flow backend sockets,
  administrator-configured mappings and bounded resources. Its RTP integration
  preserves the numeric port namespace across both hops; the generic tool may
  translate ports for other applications. See [generic relay design](udp-relay-design.md).
- Keep only configured service CIDRs routed through the first Linux profile.
  Internet exit/NAT is an explicit optional deployment policy, not a startup
  side effect. Keep IPv4/MTU 1200 for the initial RTP mode.

## Repository integration

| Existing code | Planned relationship |
| --- | --- |
| `experiments/rtp-l3` | Frozen acceptance/reference commit; extract its reusable protocol/state logic rather than maintain two implementations. Keep the historical evidence intact. |
| New `rtp-l3/` workspace crate | Portable codec/session core, packet I/O interfaces, candidate validation and recovery; initially retain the proven crypto dependency versions. |
| New `rtp-service/` workspace crate | Proposed `cpxy-rtp` client/server commands, profiles, lifecycle agent, firewalld adapter and deployment units. Names are proposals until implementation. |
| New `udp-relay/` workspace crate | Generic `cpxy-udp-relay` tool: administrator-configured UDP mappings, per-flow return routing and optional listener leases. No dependency on RTP codecs, epochs, TUN or VPN keys. |
| `server/src/main.rs` | Existing TCP proxy stays a separate service mode; an RTP endpoint is not an HTTP listener option. |
| `client/src/outbound/protocol.rs`, `cpxy-ng/src/outbound.rs` | Current `Outbound` returns byte streams. Raw L3 packet transport needs its own interface; adding an RTP URL to `ProtocolOutbound` is insufficient. |
| `mobile-engine/src/lib.rs` and `config.rs` | Existing engine terminates flows in `ipstack`. Add a distinct packet-tunnel transport mode and preserve existing proxy-mode defaults. |
| `mobile/`, OpenWrt packaging, `tsgw/` | Later platform/service adapters reuse the common transport API; no automatic change to existing profiles. |

Port admission is a shared host-service interface/module, independent of RTP
messages. The generic relay can use it without importing the VPN core.

The RTP core accepts a packet source/sink, a platform socket provider, a clock and
structured events. The service owns TUN setup, routes, secrets, port leases and
shutdown. Linux's host-socket broker remains behind the socket-provider interface;
replacement sockets on other platforms must also bypass their VPN capture.
Avoid invoking the Python experimental launcher as the shipped application.

## Work sequence and completion criteria

### A1 — Reusable core and managed service lifecycle

- [x] Extract the proven codec/state/recovery implementation into the workspace,
  retain regression coverage and keep one authoritative implementation.
- [x] Add typed client/endpoint settings, start/stop/reload handles, cancellation,
  readiness and health events. Keep the relay configuration/API protocol agnostic.
- [x] Separate packet workers from privileged TUN/firewall setup. Define
  `prepare_port`, `commit_port`, `retire_port` and `reconcile_ports` boundaries.
- [x] Add a service-owned lease journal, cleanup/restart handling and `doctor`
  output for configuration, routing, socket and firewall readiness.

Done when a Linux service starts/stops/restarts without the PoC run scripts,
closes its descriptors/TUN, and reports path repairs without restarting apps.

A1 evidence (2026-10-25): authoritative workspace crates `rtp-l3` and
`rtp-service`, historical CLI delegating to the core, and
[`rtp-service/README.md`](../rtp-service/README.md). All 26 retained codec/state/
Linux-descriptor regressions and three journal failure/restart/lock fixtures
pass. The isolated real-TUN fixture `rtp-service/tests/linux-lifecycle.sh`
passes rotation, SIGHUP reload, process restart, zero-loss post-start pings,
empty final journals and absent final TUNs. Its repair mode keeps one ping
process alive across an injected blackholed tuple and observes same-port
repairing/repaired events before successful post-repair traffic.

The packet/control ownership seam uses threads and injected providers, not an
OS privilege sandbox. A2 moves scheduled admission, retirement and reconciliation
off packet polling.
A1 originally supplied explicit administrator-managed admission; doctor labels
readiness unverified outside a running service. A2 adds managed firewalld
operations/failure integration; A3 owns service routes, DNS and backend
application reachability. The service includes independent journal cleanup and
an optional systemd ExecStopPost template, but no host deployment is claimed.

### A2 — firewalld-backed dynamic UDP ports (recommended first feature PR)

- [x] Implement the local port lease adapter and fake adapter failure fixtures.
- [x] Open the base/initial port before client readiness. For rotation, bind and
  admit the candidate before committing the epoch or emitting STATE.
- [x] Retain old sockets/rules under the protocol's existing grace/session rules;
  remove only owned retired leases. Handle reload, daemon loss and crash cleanup.
- [x] Exercise real firewalld in an isolated Linux fixture: failed add/query,
  occupied port, repeated release, reload/restart and unrelated-rule preservation.

Done when the direct endpoint rotates while exactly the live/staged ports are
managed, an admission failure leaves the current path working, and a killed
worker leaves no orphaned application rules after supervisor cleanup.

A2 evidence (2026-10-25): the protocol-independent `port-admission` crate
provides a policy-validated Unix-socket agent, bounded firewalld D-Bus operations,
canonical-rule journal and per-allocation generation checks. `rtp-service` uses
that agent in explicit managed mode. Base and initial admission finish before
readiness; scheduled preparation/commit, retirement and periodic reconciliation
run outside packet polling. Pending tasks join before supervisor cleanup. Six
shared admission/journal failure tests and all 26 retained core tests pass.

The real isolated fixture `rtp-service/tests/linux-firewalld.sh` passed (final
artifacts `/tmp/rtp-a2-JKWL`, fixture log `/tmp/rtp-a2-final-fixture.log`) with
private D-Bus and firewalld, actual TUN traffic, failed D-Bus add/query replies,
occupied sockets, preexisting-rule preparation failures with zero-loss current
traffic, paused-agent pending calls with zero-loss traffic, agent restart, two
profiles, stale/repeated release, firewalld reload/restart, and
SIGKILL followed by independent cleanup twice. Final journals/rich-rule inventory
are empty; unrelated runtime/permanent ports remain. This is a disposable local
fixture, not host deployment or remote public-path acceptance.

The worker still runs in the privileged TUN-owning service process; no OS worker
sandbox is claimed. The separate agent restricts firewall RPC to administrator
policy and a peer UID. One-second periodic reconciliation detects reload/daemon
loss instead of D-Bus signal subscriptions; brief admission outages during
reload are possible. Independent cleanup requires a working agent/firewalld;
failed cleanup stays journaled for retry, including after service-manager loss.
See [`port-admission/README.md`](../port-admission/README.md) for templates,
operation bounds and deployment prerequisites.

### A3 — RTP endpoint and application reachability

- [x] Configure an endpoint TUN pool, client address policy, service CIDRs and
  explicit return routing; optionally configure scoped SNAT when return routes
  cannot be installed. Forwarding is an explicit host deployment prerequisite.
- [x] Add a Linux client profile with tunnel identity, entry endpoint, secure key
  reference, MTU, routes and optional private DNS resolver/suffixes. Configured
  backend CIDRs take precedence over existing private/CN-direct rules in RTP
  mode; existing proxy profiles keep their defaults.
- [x] Demonstrate TCP and UDP access to an actual service behind the endpoint,
  not only ping/iperf to the endpoint's own TUN address.
- [x] Remove only profile-owned routes/DNS state on stop or failed startup.

Done when ordinary client applications reach configured backend services via
the direct endpoint and selected traffic never silently falls back to direct
Internet routing after an RTP failure.

A3 evidence (2026-10-25): optional Linux `network` profiles now configure a
canonical IPv4 tunnel pool, explicit client-address allowlist, selected service
CIDRs, dedicated policy tables/priorities, MTU 1200 and key-file/profile identity.
Endpoint/client decrypted packet policy enforces allowed source/destination
addresses. Dedicated unreachable guards persist after process failure; restart
and SIGHUP adopt guards without a direct-routing gap. Normal stop/explicit cleanup
remove exact owned policy; service-manager `cleanup-ports` preserves guards on
crash. Existing proxy profiles/defaults remain unchanged.

The real-TUN three-namespace fixture
[`linux-applications.sh`](../rtp-service/tests/linux-applications.sh) passed at
`/tmp/rtp-a3-6UGQ`: ordinary TCP and UDP applications reach a separate backend,
using explicitly provisioned forwarding and return routes. It verifies SIGKILL,
a working physical default route that must remain inaccessible for selected
traffic, admission-only post-crash cleanup, restart/SIGHUP, transport outage,
unrelated routes/rules, failed startup over a preexisting interface, repeated
cleanup and rejection of unsafe automatic DNS configuration. Three new network
policy/configuration tests and all 26 retained core tests pass. Legacy lifecycle
and same-port repair fixtures also pass (`/tmp/rtp-a1-aGKg`,
`/tmp/rtp-a1-WJ1V`).

Optional DNS is explicit `externally_managed_fail_closed` resolver/suffix metadata
only; external persistent private-suffix/no-public-fallback policy is a deployment
prerequisite, unverified by doctor. Automatic DNS mutation is deferred: per-TUN
resolved state would disappear on crash and permit suffix fallback. No DNS
success/no-leak verification or automatic DNS lifecycle is claimed. Explicit return
routing is verified; optional administrator-provisioned scoped SNAT is documented,
not managed or tested. Forwarding/firewall policy is never globally enabled by
startup. Existing experimental shared-key/data-authentication limits remain;
allowlisted addresses are not independent cryptographic client identities.
See [`rtp-service/README.md`](../rtp-service/README.md) for deployment/ownership
requirements and explicit disconnect behavior. Public deployment remains pending.

### A4 — Generic UDP relay, then fixed-port RTP integration

The generic forwarding tool can be developed independently of A3; its RTP adapter
uses the endpoint/application path once available.

- [x] Deliver a standalone relay for arbitrary configured UDP mappings, including
  port translation and generic payload fixtures, with no RTP/TUN dependency.
- [x] Forward base plus the backend's selected initial rotation port, with
  scheduled rotation disabled. Fixed mode still has a non-base data path.
- [x] Use an independent backend socket for each observed client tuple and
  ingress port; return from that same relay ingress port.
- [x] Add bounded flow creation, per-source/profile limits, idle cleanup, counters
  and authenticated administrative configuration of backend destinations.
- [x] Verify two clients, rebinding, same-port socket repair, relay restart,
  exact payload preservation and correct response demultiplexing.

A4 local completion: applications reach configured backend services through a
relay entry, the relay never loads the PSK, and backend replies remain assigned
to their original client tuple. The public-path demonstration remains A5.

A4 evidence (2026-10-25): standalone workspace crate `udp-relay` builds
`cpxy-udp-relay` with only serde/JSON/libc dependencies. Static trusted
administrator configuration supplies numeric destinations; packet sources cannot
supply them. Each mapping/generation/client tuple owns a connected upstream
socket; replies leave its original ingress. Global/mapping/source flow and
packet/byte ceilings, bounded socket/shared buffers, idle expiry and structured
counters apply in both directions. Exact zero-length datagrams work;
`recvmsg(MSG_TRUNC)` rejects whole oversized datagrams. See
[`udp-relay/README.md`](../udp-relay/README.md).

All three real socket fixtures pass: two clients/two mappings/port translation,
byte and source-port preservation, delayed old-tuple replies after rebinding,
foreign upstream rejection, zero/maximum/oversize payloads, quotas/rates, expiry,
atomic bind failure and restart. The isolated five-namespace fixture
[`linux-rtp.sh`](../udp-relay/tests/linux-rtp.sh) passes at
`/tmp/rtp-a4-K70k`: two clients reach a separate TCP/UDP backend through the
relay and RTP endpoint; both existing ping processes recover, same-port
repairing/repaired events occur after a tuple blackhole, and relay restart
recovers without restarting clients. Graceful final journals are empty and TUNs
are absent. The RTP service's optional endpoint-only `initial_rotation_port`
pins a non-base port within the existing three-or-more-port pool; startup fails
if that selected port is unavailable. The fixture disables scheduled rotation
and configures exactly base 30000 and selected non-base 30001 at the relay;
the rest of the allocation pool remains bindable.

The managed Rust `cpxy-rtp namespace-run PROFILE -- COMMAND...` launcher
also preserves host-owned outer sockets through a private inherited descriptor
broker, including replacement sockets. Its isolated real-TUN fixture
[`linux-namespace-broker.sh`](../rtp-service/tests/linux-namespace-broker.sh)
passes candidate replacement and ordinary app traffic while confirming host
routes/rules are unchanged. The child has no physical/default route and the
broker pins the configured peer IP, bind and physical device. This is an RTP
service adapter; the generic relay has no namespace or tunnel dependency.

The relay's static management authority is local OS/service configuration;
there is no remote administrative interface yet. Firewall admission remains
explicitly administrator-provisioned. Opaque mapping/generation inventory is a
foundation for A5, not an implemented lease/RPC protocol. Fixed-window rate
boundaries allow short bursts; source IP is not an authenticated client identity.
The packet worker has no OS privilege sandbox, and this bounded scanner makes no
high-throughput claim. No public host deployment or authenticated network
management is claimed by A4; A5 adds mTLS coordination and public-path evidence.

### A5 — Coordinated relay rotation and firewall lifecycle

- [ ] The RTP endpoint controller prepares the same numeric port on both hosts and
  receives a generation-bound relay readiness acknowledgement before STATE.
- [ ] Add authenticated management RPC, idempotent lease operations, staged
  deadlines, committed-lease renewal and restart reconciliation.
- [ ] Preserve base and draining paths; defer rotation when either host cannot
  prepare. Prevent stale retirement from closing a newer reuse of a port.
- [ ] Test missing/late acknowledgements, port conflicts, firewall reload,
  controller disconnect, relay crash and numeric port reuse under traffic.

Done when rotation and repair work across all three nodes, no advertised port
is unprepared on either hop, and failures have bounded recovery/cleanup.

### A6 — Application profiles and platform adapters

- [ ] Expose explicit existing-proxy versus RTP-L3 profile modes. A relayed RTP
  profile uses the relay entry plus the selected endpoint tunnel identity/key.
- [ ] Reuse status/counters for connecting, connected, repairing, degraded and
  stopped states; include effective routes and DNS policy in diagnostics.
- [ ] Add the mobile-engine packet path without passing RTP traffic through its
  TCP `Outbound`/`ipstack` proxy path. Current engine MTU validation starts at
  1280; RTP's validated IPv4 MTU 1200 needs mode-specific validation.
- [ ] Ensure every fresh socket bypasses the VPN. Preserve existing profile
  behavior and make IPv4-only support explicit; IPv6 support is separate work.

Done when a profile connects/stops and survives repair through the application
API, with defined route/DNS ownership and no secret-bearing diagnostics.

## Small reviewable implementation batches

Start with A1's port-lifecycle seam and A2's direct firewalld adapter, then deliver
the backend application path, generic relay with its RTP adapter, coordinated
rotation, and app adapters in that order. Keep extraction, firewall ownership
and relay forwarding as separate reviewable commits. Each milestone has its own
targeted checks; the completed PoC acceptance is the baseline, not a new open-ended testing gate.

Deployment-specific inputs needed before implementation testing are the selected
RTP endpoint/profile, reachable service CIDRs and return-route/SNAT choice,
public interface/zone and port-pool ownership on each host, client ingress CIDRs,
and management credentials. They belong in private configuration. This plan does
not assume new server access or publish actual deployment endpoints.
