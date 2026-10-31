# SOCKS5 kernel router implementation

Status: complete. PR: https://github.com/simophin/cpxy-ng/pull/158. Branch: `feat/socks5-kernel-router`.

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
- [x] Extend exact route export with IPv6 private/CN coverage and forced-proxy cuts.
  Extend kernel routing/lifecycle and OpenWrt settings for both address families,
  local cpxy-client or external SOCKS5, direct UDP and UDP/443 rejection.
- [x] Update integration fixtures and operator docs; run Rust, shell and available
  namespace/package checks. Record unavailable checks and remaining hardware limits.
- [x] Review final diff, commit completed stages, push branch and open a PR.

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

- Dual-family routing, OpenWrt endpoint selection, auto-enabled sibling SOCKS5,
  external-only credential-free instances, dual-stack DNS redirects and boot guards
  implemented. Small real-kernel smoke checks pass for staging, reload and recovery
  from an interrupted IPv6 selector switch. OpenWrt 24.10 init/UCI and opkg
  install/remove fixtures pass. ShellCheck and APK metadata/install tests pass.

## Validation results (2026-10-30)

| Check | Result |
|---|---|
| Rust client / mobile-engine / router CLI / GeoIP tests | 60 / 55 / 4 / 5 passed |
| Release musl builds | x86-64 and ARM64 router/client built successfully |
| Real-kernel router namespace suite | 128 assertions passed with debug binaries and release musl binaries |
| External SOCKS5 | Separate Python proxy, IPv6 endpoint, IPv4/IPv6 targets; cpxy server stopped |
| DNS integration | Both-family UDP/TCP interception, direct Chinese UDP, overseas DNS upgraded to SOCKS5 TCP, AAAA retained |
| Shared mobile TUN regression | 41 assertions passed after fixture update |
| OpenWrt 24.10 init/UCI | Passed, including external-only and client-only instances |
| OpenWrt 24.10 opkg lifecycle | Install/remove and unrelated configuration preservation passed |
| OpenWrt 25.12 APK lifecycle | Install/upgrade/enablement/config preservation/remove passed |
| APK metadata/offline installation | Architecture variants, hooks, signing and reproducibility passed |
| ShellCheck / Rust formatting / diff whitespace | Passed |
| Booted OpenWrt 24.10 / fw4 | Passed: native/proxy paths, DNS, IPv6 interception, procd/netifd/firewall reloads, failures, binary replacement, reboot, no-bypass capture and uninstall |

The namespace fixtures additionally cover forced IPv4/IPv6 proxy routes, malformed
route batches, closed stdin under procd-style events, WAN loss/recovery, missing
IPv4/IPv6 tables and TUNs, independent instances, worker/proxy failures, and recovery
from an interrupted family switch. The DNS fixture now supports persistent TCP
connections rather than closing after every answer.

Physical hardware/vendor firmware, route memory/throughput and booted fw3 firmware
are not validated for this revision. Firmware must support IPv6 DNS NAT redirection.
No live router or public cpxy server has been modified.

- Booted OpenWrt 24.10/fw4 validation completed successfully with the static
  router and real sibling cpxy-client. Continuous probes and WAN packet capture
  observed no overseas TCP fallback during reloads, gateway changes or reboot.
  Local QEMU artifacts are under `/tmp/cpxy-socks-qemu-results`; firmware assets
  under `/tmp/cpxy-socks-qemu-assets` were verified by the pinned fetcher.
- Final review preserved client-only instances without routed networks and clarified
  migration, proxy/DNS configuration and diagnostic scope. All implementation
  stages are committed and pushed. PR #158 is open against `main`.
