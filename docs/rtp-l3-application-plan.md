# RTP L3 application and relay plan

Status: proposed implementation plan, 2026-10-25. This replaces the completed
[PoC plan](rtp-l3-poc-handoff.md). The validated baseline is `451a27c`;
[Z4 acceptance](rtp-l3-z4-repair-results.md) is closed. Further ISP/soak campaigns
are not prerequisites for this phase. Checks below exercise the new features.
This change adds documentation only; it does not deploy services or alter hosts.

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

- [ ] Extract the proven codec/state/recovery implementation into the workspace,
  retain regression coverage and keep one authoritative implementation.
- [ ] Add typed client/endpoint settings, start/stop/reload handles, cancellation,
  readiness and health events. Keep the relay configuration/API protocol agnostic.
- [ ] Separate packet workers from privileged TUN/firewall setup. Define
  `prepare_port`, `commit_port`, `retire_port` and `reconcile_ports` boundaries.
- [ ] Add a service-owned lease journal, cleanup/restart handling and `doctor`
  output for configuration, routing, socket and firewall readiness.

Done when a Linux service starts/stops/restarts without the PoC run scripts,
closes its descriptors/TUN, and reports path repairs without restarting apps.

### A2 — firewalld-backed dynamic UDP ports (recommended first feature PR)

- [ ] Implement the local port lease adapter and fake adapter failure fixtures.
- [ ] Open the base/initial port before client readiness. For rotation, bind and
  admit the candidate before committing the epoch or emitting STATE.
- [ ] Retain old sockets/rules under the protocol's existing grace/session rules;
  remove only owned retired leases. Handle reload, daemon loss and crash cleanup.
- [ ] Exercise real firewalld in an isolated Linux fixture: failed add/query,
  occupied port, repeated release, reload/restart and unrelated-rule preservation.

Done when the direct endpoint rotates while exactly the live/staged ports are
managed, an admission failure leaves the current path working, and a killed
worker leaves no orphaned application rules after supervisor cleanup.

### A3 — RTP endpoint and application reachability

- [ ] Configure an endpoint TUN pool, client address policy, service CIDRs and
  explicit return routing; optionally configure scoped SNAT when return routes
  cannot be installed. Forwarding is an explicit host deployment prerequisite.
- [ ] Add a Linux client profile with tunnel identity, entry endpoint, secure key
  reference, MTU, routes and optional private DNS resolver/suffixes. Configured
  backend CIDRs take precedence over existing private/CN-direct rules in RTP
  mode; existing proxy profiles keep their defaults.
- [ ] Demonstrate TCP and UDP access to an actual service behind the endpoint,
  not only ping/iperf to the endpoint's own TUN address.
- [ ] Remove only profile-owned routes/DNS state on stop or failed startup.

Done when ordinary client applications reach configured backend services via
the direct endpoint and selected traffic never silently falls back to direct
Internet routing after an RTP failure.

### A4 — Generic UDP relay, then fixed-port RTP integration

The generic forwarding tool can be developed independently of A3; its RTP adapter
uses the endpoint/application path once available.

- [ ] Deliver a standalone relay for arbitrary configured UDP mappings, including
  port translation and generic payload fixtures, with no RTP/TUN dependency.
- [ ] Forward base plus the backend's selected initial rotation port, with
  scheduled rotation disabled. Fixed mode still has a non-base data path.
- [ ] Use an independent backend socket for each observed client tuple and
  ingress port; return from that same relay ingress port.
- [ ] Add bounded flow creation, per-source/profile limits, idle cleanup, counters
  and authenticated administrative configuration of backend destinations.
- [ ] Verify two clients, rebinding, same-port socket repair, relay restart,
  exact payload preservation and correct response demultiplexing.

Done when applications reach configured backend services through the public relay
entry, the relay never loads the PSK, and backend replies cannot be delivered to another client.

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
