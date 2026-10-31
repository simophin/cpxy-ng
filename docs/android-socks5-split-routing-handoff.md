# Android SOCKS5 split routing: implementation handoff

Date: 2026-10-31. Branch: `feat/socks5-kernel-router`.
Status: design/handoff only; Android changes are not implemented by this document.
Baseline: the SOCKS5 OpenWrt router implementation on this branch, PR #158.

## Objective and conversation decisions

Apply the router's SOCKS5 architecture to the original Android app under `mobile/`
and its Rust engine under `mobile-engine/`. Use the old cpxy protocol for cpxy
profiles; do not build this on the RTP experiment (`mobile-rtp/`, `rtp-mobile/`).

The user specifically wants Android's rootless destination-route exclusion to
bypass CN traffic in native networking. They also confirmed the design discussion
that UDP can be relayed directly inside the app without SOCKS5 UDP support.

| Traffic, for apps selected into the VPN | Intended path |
|---|---|
| CN/private TCP and UDP, unless explicitly forced into VPN | Native Android networking, excluded from TUN |
| Overseas TCP and forced-proxy TCP | TUN -> SOCKS5 CONNECT |
| Captured UDP except destination port 443 | TUN -> app-owned direct UDP socket -> destination |
| Captured UDP/443 | Reject in the engine; never send via SOCKS5 |
| Virtual VPN DNS | TUN -> concurrent split resolver; resolver connections follow direct/proxy policy |

Apply the design to IPv4 and IPv6. Plaintext, no-auth SOCKS5 CONNECT is sufficient;
no UDP ASSOCIATE, proxy authentication, or outer SOCKS TLS is required.

**Route exclusion also bypasses the app's packet filter.** Consequently CN
UDP/443 excluded from TUN is allowed natively. The app can block UDP/443 that
enters TUN, but must not claim a global HTTP/3 block with CN exclusions enabled.
This is the implication of the discussed architecture, not a root requirement.
UDP/443 is a port-based QUIC block, not HTTP/3 payload detection.

Ordinary UDP is always direct, including when its destination matches a TCP
forced-proxy override. Capturing such UDP does not mean proxying it. Preserve DNS
interception before the ordinary UDP relay. TCP/853 should follow normal TCP
routing for the new mode rather than retaining the mobile filter's blanket block.

## Recommended app architecture

Use one SOCKS5-based TCP relay path for both profile types:

1. **Cpxy profile:** run a loopback SOCKS5 listener inside the app process, backed
   by `ProtocolOutbound`. The packet engine uses that endpoint. Reuse the existing
   protocol implementation; do not spawn a standalone executable or require a
   second installed app. Bind the internal listener to loopback on an ephemeral
   port, separately from optional user-facing HTTP/SOCKS proxy listeners.
2. **External SOCKS5 profile:** connect the engine directly to the configured
   reachable endpoint. No cpxy server URL or key is required for this profile.

These profile/UI details are implementation recommendations from the discussion;
the firm requirements are rootless CN bypass, a SOCKS5 TCP path, direct UDP, and
split DNS. Preserve existing profile IDs and cpxy credentials when migrating saved
profiles. Old profiles should decode as cpxy profiles automatically. Keep secrets
out of logs, diagnostics and generated handoff/test artifacts.

Create the embedded listener before DNS initialization can dial it. It and the
packet worker must share a clear start/stop lifecycle: bind failures fail startup,
partial startup cleans up, switching profiles closes old connections/listeners,
and stopping releases the TUN descriptor and sockets. Avoid deadlocking a runtime
while synchronous UniFFI startup waits for an async listener on the same thread.

The app UID is already excluded from its own VPN by `AppSettings.routing()`.
Preserve that in both per-app allow-list and deny-list modes, so the embedded
cpxy transport, external SOCKS sockets, direct DNS and UDP sockets do not loop
back into TUN. If socket ownership changes, use an explicit protected-socket
mechanism; do not assume an arbitrary new process inherits the app's bypass.

## Android route implementation

The app currently supports API 26+. `VpnService.Builder.excludeRoute(IpPrefix)`
is available from API 33 and supports both families. On API 33+, start with the
appropriate catch-all VPN routes, exclude exact eligible CN/private prefixes,
and retain forced-proxy destinations inside VPN. Use the embedded reviewed data
shared by DNS and the router; do not fetch a separate country list.

Compile direct prefixes **after subtracting forced-proxy CIDRs**. Do not blindly
exclude the whole CN set and accidentally let a more-specific forced-proxy
address bypass VPN. Broad and overlapping exclusions must work in both families.

Keep `10.233.0.2/32`, the existing virtual DNS address, explicitly included in
VPN. It is inside private `10.0.0.0/8`, which otherwise bypasses TUN. Apply the
longest-prefix rules deliberately, and reserve virtual DNS before treating any
prefix as a bypass candidate. Preserve IPv6 Full/Direct/Off semantics; an Off
profile must not accidentally re-enable IPv6 through excluded CN prefixes.

For APIs 26-32, use positive `addRoute` prefixes covering the complement of the
direct set. Add a correct IPv6 complement compiler; the existing IPv6 export is
a direct-prefix list, not a complement. Do not raise minSdk or drop IPv6 to avoid
implementing compatibility without an explicit product decision.

Measure actual route counts, VPN setup time and Binder/platform limits on both
paths. Do not assume thousands of routes always fit. If exact plans cannot be
installed, use a conservative capture plan or full-TUN fallback with CN/private
connections relayed directly in userspace. This fallback may capture extra CN
traffic, but must never let overseas/forced-proxy TCP bypass VPN. Record the
fallback and reason in status/logs. Do not silently coarsen a CN exclusion so that
foreign addresses become direct.

`geoip_data::routes::tunnel_ipv4(proxy, longest_direct)` already provides a
conservative IPv4 capture plan: limiting `longest_direct` moves narrow CN ranges
into TUN. Such a plan requires userspace CN direct routing; pairing it with the
router's unconditional proxy mode would incorrectly proxy those CN connections.
The UDP/443 guarantee remains limited to packets actually captured by VPN.

## DNS and UDP

Reuse the current `Racer`, cache, search domains, IPv6 answer handling and virtual
DNS server. Keep the existing Android DNS interception scope unless deliberately
changing it: `start()` currently intercepts the virtual resolver, whereas the
Tailscale gateway intercepts DNS to any address. Do not accidentally lose virtual
DNS by copying the OpenWrt socket-listener startup path.

For resolver connections, reuse the router's userspace classification:
CN/private resolvers direct, overseas/forced resolvers through the selected
SOCKS5 endpoint. Convert proxied plain UDP resolver specs to DNS-over-TCP; support
DoT/DoH without weakening certificate validation. LAN-facing/app-facing DNS can
remain UDP. Preserve explicit legacy `?via=cpxy` settings as a proxy override for
old profiles, and preserve the distinction from the separate legacy cpxy engine.
Hostname resolver endpoints need a bootstrap path that cannot recurse into VPN
DNS; retain pinned `?ip=` and explicit proxy resolution behavior.

The existing `handle_udp()` / `relay_udp()` already maintain direct UDP sockets
per captured flow and return replies through ipstack/TUN. Reuse them. Preserve
flow isolation, datagram boundaries and idle cleanup; cover zero-length datagrams
and large payload behavior when reviewing this code. No SOCKS UDP support or
server-side UDP change is needed.

Native-bypass traffic uses the phone's underlying IPv6 provisioning. Direct UDP
sockets similarly need usable underlying IPv6. Do not copy the router's ULA/NAT
configuration to Android or advertise connectivity the underlying network lacks.

An external proxy must be reachable outside this VPN. In particular, do not assume
the router's Tailscale-only `100.x` endpoint is reachable on a phone: Android
allows one active VPN service per user/profile, so a separate Tailscale VPN cannot
simply run alongside this app. Use an endpoint reachable through the underlying
network; integrating a tailnet transport would be separate work.

## Relevant code and traps

Paths below are relative to the repository root.

| File | Reuse/change |
|---|---|
| `mobile/androidApp/src/main/kotlin/dev/fanchao/cpxy/vpn/CpxyVpnService.kt` | Currently adds `0.0.0.0/0` and `::/0`; install the computed route plan here and preserve app routing/lifecycle. |
| `mobile/shared/src/commonMain/kotlin/dev/fanchao/cpxy/vpn/Profile.kt` | Currently requires `server`; add compatible profile/backend configuration and keep `EngineConfig` JSON in sync with Rust. |
| `mobile/shared/src/commonMain/kotlin/dev/fanchao/cpxy/vpn/ProfileRepository.kt` | Profile persistence/import/export and migration. |
| `mobile/shared/src/commonMain/kotlin/dev/fanchao/cpxy/vpn/ui/EditProfileScreen.kt` | Profile type, cpxy credentials or external endpoint validation. |
| `mobile/shared/src/commonMain/kotlin/dev/fanchao/cpxy/vpn/Settings.kt` | App UID bypass, per-app selection, forced routes and existing optional listeners. |
| `mobile-engine/src/config.rs` | Strict JSON parser (`deny_unknown_fields`); backend settings must match Kotlin. |
| `mobile-engine/src/ffi.rs` | UniFFI startup takes TUN ownership; expose route planning before establishing VPN if needed. Regenerate bindings via existing build tasks. |
| `mobile-engine/src/lib.rs` | `start()` still uses cpxy; `start_router()` uses SOCKS5 but OpenWrt DNS sockets. Refactor shared startup rather than calling `start_router()` from Android. Reuse UDP relay. |
| `mobile-engine/src/routing.rs` | `Router::socks5()` sets `always_proxy=true`. Correct for exact OpenWrt routing, unsafe for conservative Android capture unless a direct fallback is added. |
| `mobile-engine/src/dns.rs` | `router_handler()` / `router_spec()` provide SOCKS5 DNS routing. Retain mobile virtual DNS interception. |
| `mobile-engine/src/filter.rs` | Existing mobile mode blocks TCP/853. Make new-mode policy explicit; do not use the `router` boolean as an accidental Android mode selector. |
| `mobile-engine/src/proxy.rs` | Existing SOCKS handshaker/listener plumbing. Its normal listener wraps the split router; the embedded cpxy backend listener must use pure `ProtocolOutbound`. |
| `client/src/outbound/socks5.rs` | Plaintext no-auth CONNECT, bounded negotiation, IPv4/IPv6/domain addressing and initial payload handling. Preserve resolved destination IPs. |
| `client/src/socks_proxy_server.rs` | Existing SOCKS CONNECT server handshake. |
| `geoip-data/src/routes.rs` | Exact IPv4 direct/compiler and conservative tunnel plan; IPv6 CN export and `router_direct_ipv6(proxy)` include private ranges and forced cuts. |
| `mobile/shared/src/commonTest/kotlin/dev/fanchao/cpxy/vpn/` | Existing profile/repository/settings tests to extend. |
| `mobile-engine/test/tun-lab.sh`, `mobile-engine/test/dns.py` | Existing real-TUN regression and dual-family persistent-TCP DNS fixture. |
| `packaging/openwrt/test/lab.sh`, `socks5-fixture.py` | Reusable controlled SOCKS/DNS fixtures; preserve OpenWrt behavior while refactoring shared code. |

Keep shared iOS/Tailscale/OpenWrt consumers working. New serialized profile fields
may affect shared iOS code even when Android is the implementation target. Do not
silently force those platforms into Android's route installation mechanism.

## Suggested work sequence

- [ ] Inspect current branch changes and applicable instructions; record a short
  implementation plan before edits. Do not overwrite unrelated user work.
- [ ] Add a shared, testable IPv4/IPv6 route-plan interface, virtual DNS reservation,
  forced-route subtraction, and conservative capture fallback.
- [ ] Add backend/profile migration and validation; preserve IDs, import/export,
  selected profile and optional user-facing proxy settings.
- [ ] Implement lifecycle-owned embedded cpxy SOCKS listener and external SOCKS mode.
- [ ] Wire Android API 33+ exclusion and API 26-32 inclusion paths through UniFFI;
  retain direct handling for any CN traffic captured by a conservative plan.
- [ ] Reuse direct UDP and split DNS with the new backend; align TCP/853 and
  captured UDP/443 filtering without changing unrelated legacy consumers.
- [ ] Add focused tests, build the APK and validate on emulator/device. Update this
  handoff/checklist with completed work, evidence and remaining limitations.

Commit coherent stages on this branch and update its PR when instructed by the
working session. This handoff does not authorize changing the deployed TR3600,
any remote proxy host, or distributing/installing an APK on a user's device.

## Validation and acceptance

1. Route coverage: dataset boundaries, random IPv4/IPv6 addresses, private ranges,
   overlapping/broad forced routes, virtual DNS, and no foreign bypass under any
   conservative plan. Test both Android API branches and route setup limits.
2. Profiles: old JSON migrates; external SOCKS needs no cpxy credentials; invalid
   endpoint/auth/TLS forms fail clearly; credentials stay redacted.
3. TCP: cpxy-backed and external SOCKS modes, original IPv4/IPv6 destination
   preservation, native CN path, captured CN fallback, and no overseas direct
   fallback when proxy/listener fails.
4. UDP: ordinary overseas IPv4/IPv6 UDP works while SOCKS is unavailable and
   causes no SOCKS CONNECT/UDP ASSOCIATE. Captured UDP/443 is rejected. Verify and
   describe the excluded-CN UDP/443 exception rather than claiming it is blocked.
5. DNS: virtual DNS reachable despite private exclusions; concurrent CN/overseas
   queries, cache/search behavior, AAAA, proxy TCP DNS from a UDP spec, and DoT/DoH.
6. Lifecycle: stop/restart/profile switch, partial startup, app allow/deny lists,
   Wi-Fi/mobile handover, IPv6 loss, background operation and always-on/lockdown
   behavior. Test route refresh after policy changes without transient foreign
   direct bypass. Capture the underlying interface to prove proxy/direct paths.
7. Observability: native-bypass flows are absent from engine counters/history;
   explain that scope. Existing app traffic/updates APIs must continue working.

Useful baseline commands (check `mobile/README.md` and CI for toolchain setup):

```sh
cargo test --locked --offline -p client -p mobile-engine -p geoip-data
cd mobile
./gradlew :shared:allTests :androidApp:assembleDebug
```

Use existing cargo-ndk/UniFFI Gradle integration, not hand-written generated
bindings. Baseline router validation is recorded in
[socks5-kernel-router-plan.md](socks5-kernel-router-plan.md); it does not establish
that the Android changes described here have been implemented or tested.

## Platform references

- [Android VPN Builder](https://developer.android.com/reference/android/net/VpnService.Builder):
  `excludeRoute` API 33, destination-prefix routing, and longest-prefix precedence.
- [Android VPN guide](https://developer.android.com/develop/connectivity/vpn):
  VPN lifecycle, outbound socket handling, per-app behavior and the single active
  VPN service per user/profile restriction.
