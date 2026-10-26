# RTP L3 application plan

Status: direct Linux service and packet-engine API implemented. The generic UDP
relay and cross-host port coordination were removed on 2026-10-25.

## Current architecture

```text
Client apps / TUN -> RTP over UDP -> RTP endpoint / TUN -> service networks
```

The client connects directly to the RTP endpoint. The endpoint owns its base and
rotation sockets, local firewall admission, tunnel interface and selected IPv4
service routes. Port rotation and same-port repair remain supported, with no
remote listener leases, relay management certificates or two-host commit.

If a middle server is needed to improve the route, it will terminate RTP itself.
An ordinary routed VPN such as WireGuard or Tailscale can carry onward traffic
from that server to destination networks. That forwarding deployment is deferred;
this change does not configure a second VPN, routes or masquerade for it.

## Implemented scope

- Reusable `rtp-l3` codec/session/recovery core and managed `cpxy-rtp` service.
- Local UDP admission, exact owned-rule journaling, reconciliation and cleanup.
- Selected IPv4 service CIDRs, packet allowlists and fail-closed routing guards.
- Direct backend TCP/UDP access, explicit return routes, and optional managed
  firewalld masquerade as documented in the service README.
- Namespace client with host-owned outer sockets, including repair sockets.
- `rtp-mobile`, the Android app's engine: internet-exit client with the DNS
  split, socket protection and status counters (see the
  [RTP mobile plan](rtp-mobile-plan.md)). It replaced the earlier
  `mobile-engine` packet API.

See the [service README](../rtp-service/README.md) for the current CLI and
[protocol specification](rtp-l3-protocol.md) for framing and security limits.
The historical experiment CLI delegates to the workspace core.

## Validation

Direct Linux namespace fixtures cover lifecycle, rotation, same-port repair,
backend applications, routing guards, socket brokering, firewalld reconciliation
and crash cleanup. The app engine's fixture
[`linux-lab.sh`](../rtp-mobile/tests/linux-lab.sh) runs it against an
internet-exit endpoint and checks routing, the DNS split, refusals, roaming,
fail-closed behaviour and stop.

Removal validation (2026-10-25): 97 unit/integration tests passed across
`rtp-l3`, `rtp-service`, `port-admission` and `mobile-engine`. Linux builds and
all five direct namespace checks passed: packet API, lifecycle/rotation,
same-port repair, applications/fail-closed routing and namespace broker.
Temporary validation processes and staging were removed.

Earlier A4/A5/A6 relay acceptance established scoped public-path application
behavior, including relay restart and rotation. Those deployments were temporary
and removed. The retained [A5 results](rtp-l3-a5-results.json) are historical
experimental evidence, not a description of the current architecture. The relay
crate, management API, deployment templates and relay fixtures are removed.
No throughput, zero-loss or sustained-load guarantee is claimed.

## Remaining work

- iOS NetworkExtension for the RTP app.
- Persistent private DNS with no public fallback; current DNS configuration is
  metadata and requires external policy.
- IPv6 and a supported full-internet exit mode; current RTP mode is IPv4,
  MTU 1200, selected service networks.
- Operational deployment, worker privilege isolation and targeted load testing.
- Revisit experimental shared-key and unauthenticated-data security choices if
  production requirements call for authenticated data, replay protection,
  independent client identities or forward secrecy.
- Configure the onward VPN and forwarding separately when deployment is chosen.

Private endpoints, credentials and host paths do not belong in the repository.
