# SOCKS5 kernel router implementation

Status: in progress. Branch: `feat/socks5-kernel-router`.

## Agreed behavior

- Kernel routing forwards CN/private TCP directly for IPv4 and IPv6. Other TCP
  enters TUN and always uses a required plaintext, unauthenticated SOCKS5 proxy.
- All UDP goes directly except UDP/443, which is rejected for both families.
  Client DNS on UDP/TCP 53 is redirected to the split resolver.
- Split DNS retains concurrent Chinese/overseas queries, regional answer selection,
  caching, search domains and local dnsmasq handling. Resolver destinations follow
  the same regional/forced-proxy policy in userspace; proxied DNS uses TCP/DoT/DoH.
- The SOCKS5 endpoint may be an external proxy or the existing cpxy-client.
  Router-originated proxy/bootstrap sockets use normal system routing.
- Shared mobile/Tailscale consumers retain their existing cpxy interfaces.
- Preserve route generations, startup guards and protection against overseas TCP
  falling back to WAN when the worker, proxy, TUN or routes fail.

## Commit stages

- [x] Inspect implementation and record this plan before code changes.
- [x] Implement no-auth SOCKS5 CONNECT (IPv4/IPv6/domain), bounded handshake,
  meaningful errors and destination preservation; validate against a local fixture.
- [x] Wire SOCKS5-only router CLI, proxy-only TUN TCP and split DNS routing without
  changing mobile/Tailscale defaults. Validate CLI, DNS transport and relay behavior.
- [ ] Extend exact route export with IPv6 private/CN coverage and forced-proxy cuts.
  Extend kernel routing/lifecycle and OpenWrt settings for both address families,
  local cpxy-client or external SOCKS5, direct UDP and UDP/443 rejection.
- [ ] Update integration fixtures and operator docs; run Rust, shell and available
  namespace/package checks. Record unavailable checks and remaining hardware limits.
- [ ] Review final diff, commit completed stages, push branch and open a PR.

## Validation and decisions

The existing IPv4 GeoIP suite is reusable but assumes cpxy inside the router,
IPv6 disabled and TCP/853 blocked. These assumptions must be revised. UDP/443 is
a port-based QUIC block, not HTTP/3 payload inspection. TCP/853 follows ordinary
TCP splitting under the agreed policy. No cpxy wire-protocol/server changes.

Physical router throughput, vendor firmware and hardware offload behavior require
hardware validation; software/hardware flow offload remains disabled for this mode.

## Progress

- Initial inspection: existing kernel GeoIP mode and DNS racer are reusable;
  external SOCKS5 outbound and dual-family kernel policy are the main gaps.

- SOCKS5 fixture tests pass (2 tests): IPv4/IPv6/domain targets, initial payload,
  variable reply addresses, authentication refusal and CONNECT failures. Local
  socket tests require elevated execution because sandbox socket binds are denied.

- Router/DNS integration and IPv6 export tests pass: exact dataset boundaries and
  50,000 IPv6 samples, forced exclusions, SOCKS5 DNS wire fixtures for both
  address families, proxy-only TUN behavior and required CLI endpoint. Existing
  mobile-engine tests continue to pass. Kernel/package validation is next.
