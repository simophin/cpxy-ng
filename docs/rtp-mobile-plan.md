# RTP mobile VPN app (Android): plan

Status: phases 0–2 done (2026-10-26). Android only; iOS is left possible but not built. See
[Validation](#validation) for what was checked.

## Goal

A phone app that does what an OpenWrt router with an RTP internet exit does: connect to a
`cpxy-rtp endpoint --internet-exit` and send the phone's traffic through the RTP tunnel, while
the local region and private networks stay direct.

1. **Traffic split.** IPv4 traffic to the local region and private ranges goes direct; all
   other IPv4 goes through the tunnel.
2. **DNS split**, the same upstream-first policy the router uses (`client::dns_split::Racer`):
   upstream (local-region) resolvers, which the user can change, are asked directly;
   alternative resolvers are asked **through the tunnel**.
3. **IPv6** is not carried, since the tunnel's inner packets are IPv4 only. It is captured and
   refused so it cannot leak around the tunnel, and AAAA answers are dropped.
4. **App split**, as in the proxy app: all apps, only selected apps, or all apps except selected.

Out of scope for the first version: selected-service (non-internet) mode, the quick settings
tile, always-on VPN, the self-updater, a traffic log, iOS.

## Decisions

- **A new app**, application ID `dev.fanchao.cpxy.rtp`, name "cpxy RTP". It installs next to the
  proxy app (`dev.fanchao.cpxy.vpn`); the OS allows one active VPN at a time.
- **Its own Gradle build** in `mobile-rtp/`, independent of `mobile/`. Code the proxy app has
  that this app needs (the app picker, per-app settings) is copied, not shared.
- **A new Rust crate `rtp-mobile`** for the native library. The RTP app does not ship ipstack or
  the proxy engine (it uses only the client crate's DNS split), and the proxy app does not ship
  the RTP worker. The unused RTP packet API in
  `mobile-engine` (`src/packet.rs`, its UniFFI exports, `examples/rtp-packet.rs` and
  `tests/linux-rtp-packet.sh`) moves into the new crate.
- Same UI stack as the proxy app: Kotlin Multiplatform with Compose Multiplatform (only the
  Android target for now, UI in `commonMain` so an iOS target can be added), UniFFI bindings,
  cargo-ndk. Kotlin package of the bindings: `dev.fanchao.cpxy.rtp.engine`.
- The password is never stored. The app derives the 32-byte key (`rtp_l3::derive_psk`,
  Argon2id) when a profile is saved and keeps only the key, encrypted with an Android Keystore
  key. The engine takes the key directly.

## Architecture

```text
┌────────── KMP (Compose Multiplatform) ──────────┐
│ shared: UI, profiles, settings, VpnController    │
└──────────────────────────────────────────────────┘
       │
       ▼
 RtpVpnService (Kotlin): builds the TUN with the engine's routes, hands over the fd
       │
       ▼
 Rust `rtp-mobile`
   TUN ⇄ packet shim ⇄ socketpair ⇄ rtp-l3 client worker ⇄ UDP to the endpoint
          ├─ UDP 53 to the virtual DNS address → DnsSplitHandler
          │     upstream: direct sockets
          │     alternative: UDP packets injected into the tunnel
          ├─ IPv6 → ICMPv6 administratively prohibited
          ├─ UDP 443 → ICMP port unreachable (QUIC does not fit MTU 1200)
          ├─ TCP 853, TCP 53 to the DNS address → ICMP port unreachable
          └─ other IPv4 from the tunnel address → tunnel
```

### Routing split in the OS

Direct traffic never enters the TUN: the TUN's routes are the complement of the direct set
(`geoip_data::routes::tunnel_ipv4`). Kernel routing costs nothing per packet and keeps the
engine a packet pump; deciding in the engine would need a userspace TCP stack, which is the
proxy app's design.

The exact complement is about 18,000 prefixes, too many to hand to `VpnService.Builder`. Direct
prefixes longer than /18 are left in the tunnel, which brings the list to about 2,100 routes and
sends about 2.4% of local-region addresses through the tunnel. Coarsening only ever moves direct
addresses into the tunnel, never the reverse. The same list is used on every Android version
(`addRoute` only; no `excludeRoute`, which needs API 33).

- The virtual DNS server is `198.18.0.53`, in the benchmarking range, which is not direct and so
  is already inside the tunnel routes. The engine answers it and never sends it to the endpoint.
- The TUN address is the profile's tunnel address (`/32`), which must be one of the endpoint's
  `--client-address` values. All clients share the key, and two devices using the same address
  replace each other's session; the profile editor says so.
- IPv6: the TUN has no IPv6 address but routes `::/0`, as the proxy app's IPv6-off mode does, so
  IPv6 cannot leak around the tunnel and apps fall back to IPv4.
- The app's own package is always left out of the VPN (`addDisallowedApplication`, or simply not
  listed with "only selected apps"), so the RTP sockets and the upstream DNS sockets bypass the
  tunnel without per-socket work. The engine still calls `protectSocket` for each RTP socket, as
  the packet API does today.

### Packet shim

The `rtp-l3` worker polls one packet descriptor. The engine gives it one end of a
`SOCK_DGRAM` socketpair, and a shim on the engine's tokio runtime sits between the TUN and the
other end, so `rtp-l3` needs no change for it:

- From the TUN: answer UDP DNS to the virtual address; reply with ICMP errors as in the diagram;
  drop anything not from the tunnel address, multicast and broadcast; pass the rest on.
- From the worker: replies to injected DNS queries go to the tunnel DNS upstream; everything
  else addressed to the tunnel address is written to the TUN.
- Injection: the tunnel DNS upstream writes crafted IPv4/UDP queries (from the tunnel address,
  source ports 61000–65535, above Linux's ephemeral range) to the socketpair.

The platform's 4-byte packet-information header (iOS utun) is handled in one place, so an iOS
port only flips a flag.

### DNS

- `DnsSplitHandler` with an in-memory cache and IPv6 off (no AAAA answers).
- Upstream resolvers: any `client::dns_split::spec` server, asked directly. Defaults are the
  router's local-region defaults.
- Alternative resolvers: plain IPv4 addresses, asked over UDP through the tunnel with an EDNS0
  buffer of 1232 bytes. Default `8.8.8.8` and `1.1.1.1`.
- Private DNS (DNS over TLS on TCP 853) is refused so Android falls back to the VPN's DNS.

### Roaming

The engine resolves the endpoint (a name, IPv4 or IPv6 literal) with the system resolver when it
starts. On a network change, the service calls
`networkChanged()`: the engine asks the worker for an immediate repair (a fresh socket and a
PING) instead of waiting for PING timeouts. That needs one hook in `rtp-l3`: a
`SocketProvider::repair_requested()` default method the client loop checks. The service also
sets the VPN's underlying network.

The service follows the best network that is not a VPN (`registerBestMatchingNetworkCallback`,
or `requestNetwork` below API 31), not the default network: once the VPN is up, Android reports
it as the app's default network although the app is outside it, and reports no further changes.

### Engine API (UniFFI)

- `startRtpEngine(tunFd, profileJson, key, platform): RtpEngine`, throwing a fixed
  `RtpException.Failed` message that never includes key or OS detail.
- `RtpEngine.stop()`, `RtpEngine.networkChanged()`, `RtpEngine.status(): RtpStatus` (state,
  last error kind, packets and bytes each way, repairs, fallbacks).
- `tunnelRoutes(): List<String>`, `dnsAddress()`, `deriveKey(password)`,
  `validateProfile(profileJson)`.
- `RtpPlatform.protectSocket(fd): Boolean`.

Profile JSON:

```json
{
  "endpoint": "vpn.example.net:30000",
  "address": "10.88.0.2",
  "dns_upstream": ["223.5.5.5"],
  "dns_alternative": ["8.8.8.8"]
}
```

### Fail-closed behaviour

While the VPN is up, the TUN and its routes stay installed even when the tunnel is down: traffic
that would go through the tunnel fails, never goes direct. A worker that ends with an error is
restarted by the service with backoff until the user disconnects.

### App

- One screen: connect/disconnect, status (connecting, connected, repairing, degraded), traffic
  counters, the profile list, and a settings entry for the app split.
- Profile editor: name, endpoint, tunnel address, password (write-only: replaced, never shown),
  upstream and alternative DNS lists. Validation runs in Rust.
- `RtpVpnService`: foreground service (`specialUse`, subtype `vpn`) with a disconnect action in
  its notification.

## Phases

### Phase 0: `rtp-mobile` crate, tested on Linux

- Workspace member `rtp-mobile/` (`cdylib`, `staticlib`, `rlib`) with the engine above, the
  `rtp-l3` repair hook, and the packet API removed from `mobile-engine`.
- `rtp-mobile-linux`: creates a TUN, installs the engine's routes, runs the engine; for the lab.
- Unit tests: profile validation, routes, shim classification, DNS injection framing.
- Namespace lab against a real `cpxy-rtp endpoint --internet-exit`: tunnelled TCP and UDP reach
  an "internet" backend from the endpoint's address; a direct-set address goes direct and never
  reaches the endpoint; DNS answers come from the upstream for local-region names and from the
  alternative resolver through the tunnel otherwise; IPv6 and UDP 443 get ICMP errors;
  `networkChanged` recovers after the client's outer address changes; with the endpoint gone,
  tunnelled traffic fails rather than going direct; stop closes the TUN.

### Phase 1: Android app

- `mobile-rtp/` Gradle build: `shared` and `androidApp`, JVM tests for profiles and settings.
  The app keeps the proxy app's per-app split; the tile, always-on VPN, updater and traffic log
  are left out.
- `librtp_mobile.so` for the four ABIs and the Kotlin bindings, as declared task outputs.
- Emulator check against a test endpoint: connect; the public IP is the endpoint's; a
  local-region address goes direct; the DNS split answers both ways; app split includes and
  excludes the chosen apps; the tunnel recovers after switching networks; disconnect removes
  the TUN. Measure how long `establish()` takes with the route list.

### Phase 2: CI

- Pull requests: `cargo test -p rtp-mobile`, the namespace lab, `:shared:allTests` in
  `mobile-rtp/`.
- main and tags: the release APK, checked for the native library in all four ABIs, uploaded as
  a CI artifact.

## Validation

2026-10-26:

- Unit tests: `rtp-mobile` (profile, packet classification, routes, status), `rtp-l3`,
  `rtp-service`; `:shared:allTests` in `mobile-rtp/`.
- `rtp-mobile/tests/linux-lab.sh` against a real `cpxy-rtp endpoint --internet-exit`: tunnelled
  TCP and UDP, direct traffic outside the endpoint, the DNS split both ways, no AAAA answers,
  ICMP refusals of QUIC and DNS over TLS, recovery after the client's address changes, fail
  closed with the endpoint killed, and the TUN closed on stop. With `networkChanged` the first
  connection after the address change succeeds in about 1.1 s, against 2.1 s without it; the lab
  requires under 1.6 s. The existing lifecycle tests, with and without repair, still pass.
- API 36 emulator against a temporary internet-exit endpoint on a public server, debug and
  release APKs: `establish()` with 2,105 routes took 46–88 ms; the browser saw the endpoint's
  public address; DNS answers came from the alternative servers through the tunnel; a
  local-region address never reached the endpoint while others did; with Chrome in "All apps
  except selected" the VPN's UID ranges left out Chrome and the app, and Chrome saw the direct
  address; switching Wi-Fi off and on was detected both ways and lost one ping; with the
  endpoint frozen, traffic failed instead of going direct and the app said the endpoint was not
  answering, then recovered about 2 s after it resumed; disconnecting removed the TUN.

Not yet checked: a physical phone on a real cellular network, battery use, and long-running
sessions.

## Risks

1. **Route count.** About 2,100 routes; `establish()` took under 0.1 s on the emulator, still to
   measure on a real phone. The `/18` limit
   is one constant if it needs to change.
2. **QUIC and MTU.** QUIC needs 1200-byte UDP payloads, which do not fit an inner MTU of 1200,
   hence the UDP 443 refusal. Other UDP with large datagrams relies on IP fragmentation.
3. **Shared key.** Every phone holds the same key and can impersonate any other client. Per-client
   keys and automatic address assignment need protocol work and are out of scope.
4. **Battery.** The worker sends a PING when idle for its keepalive interval (15 s here, the
   protocol default); measure on cellular with the screen off.
