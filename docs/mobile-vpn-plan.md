# Mobile VPN app (Android + iOS): plan

Status: Phases 0–2 (engine, Android MVP and APK CI) done; Phase 3 (iOS) next. Work through the
phases in order and update this file as decisions change.

## Goal

A new VPN app for Android and iOS that does on the phone what the `cpxy-router` OpenWrt package does
on the router (see [packaging/openwrt/README.md](../packaging/openwrt/README.md)):

1. **DNS split.** The app answers the device's DNS with `dns_split`: an answer from the `upstream`
   servers is used when all its addresses are in CN, otherwise the `alternative` servers' answer.
2. **Traffic split.** TCP to CN (and private) IPv4 addresses goes direct; everything else goes
   through the cpxy server.
3. **UDP.** UDP 443 (QUIC) is refused so browsers fall back to TCP through the proxy. All other UDP
   goes direct.
4. IPv6 is refused, so apps fall back to IPv4 (the GeoIP data is IPv4 only).

Decisions already made:

- UI in Kotlin Multiplatform (Compose Multiplatform); engine in Rust.
- A **new** app. The existing `client/android-app` is left untouched.
- Location `mobile/` at the repository root; application ID `dev.fanchao.cpxy.vpn`, so it installs
  next to the existing app.
- Routing is **CN-direct / proxy-everything-else only**. No AI or Tailscale divert servers.
- Packets are handled with the **`ipstack`** crate directly, not the tun2proxy library: no local
  SOCKS port that other apps on the device could use.
- No paid Apple Developer account yet. iOS is built in CI (unsigned) and tested later through
  TestFlight, which needs the paid Apple Developer Program membership; the Network Extension
  (packet tunnel) capability is available to any paid account.

## What carries over from the router

| Router piece | On mobile |
|---|---|
| `client::dns_split` (`Racer`, `DnsSplitHandler::handle`, `DnsCache::open_in_memory`) | Reused. `handle(&Message) -> Message` is transport-independent, so DNS packets read from the TUN are fed to it. Use the in-memory cache. |
| `client::outbound` (`IPDivertOutbound`, `DirectOutbound`, `ProtocolOutbound`, `StatReportingOutbound`) | Reused. Compose them like `cn_outbound` but without the AI/Tailscale branches: private, loopback, link-local or CN → direct, else proxy. |
| tun2proxy (TUN → SOCKS5) | Replaced by `ipstack`: each TCP flow from the TUN goes straight into the `Outbound`. |
| Firewall rejects UDP 443; other UDP leaves via WAN | Done in the engine: UDP 443 is answered with ICMP port unreachable, other UDP is relayed through direct sockets. |
| IPv6 refused | Route `::/0` into the TUN; the engine answers with ICMPv6 administratively prohibited. On Android the TUN has no IPv6 address, so apps do not try IPv6 at all and the route only keeps it off the underlying network. |
| Fail closed when the proxy dies | The TUN stays up while the engine runs; a proxy failure fails the connection, never sends it direct. |

### Avoiding routing loops

The engine's own sockets (direct connections, the cpxy server, upstream DNS) must not re-enter the
tunnel.

- **Android:** `VpnService.Builder.addDisallowedApplication(<own package>)`. Every socket the app
  opens then bypasses the VPN without per-socket `protect()`.
- **iOS:** sockets opened by the packet tunnel extension already bypass its own tunnel.

## Architecture

```text
┌───────────── KMP (Compose Multiplatform) ─────────────┐
│ shared/  UI, profile storage, VpnController (expect)   │
│   androidMain: VpnService start/stop, status           │
│   iosMain:     NETunnelProviderManager start/stop      │
└────────────────────────────────────────────────────────┘
        │ Android: same process     │ iOS: separate extension process
        ▼                           ▼
  CpxyVpnService (Kotlin)     PacketTunnelProvider (Swift, thin)
        │  tun fd + config JSON     │  tun fd + config JSON
        └──────────► Rust `mobile-engine` ◄──────────┘
                     ipstack ─┬─ TCP  → outbound (direct | proxy)
                              ├─ UDP/TCP 53 to virtual DNS IP → DnsSplitHandler
                              ├─ UDP 443 → drop / ICMP unreachable
                              └─ other UDP → direct relay (NAT table, idle timeout)
```

### Rust: `mobile-engine` crate

- New workspace member at `mobile-engine/` (added to the root `Cargo.toml`), depending on `client`
  with the `dns-split` feature and on `ipstack`.
- `crate-type = ["cdylib", "staticlib", "rlib"]`: `.so` for Android, xcframework for iOS.
- Refusals are decided per packet in `filter.rs`, in front of ipstack, which cannot write raw
  packets back. UDP 443 and TCP 853 get ICMP port unreachable. IPv6 gets ICMPv6
  *administratively prohibited*, not *no route*: Linux UDP sockets ignore the soft no-route error
  unless they ask for ICMP errors, so UDP apps would wait for a timeout. ICMP, multicast and
  broadcast are dropped silently.
- Idle timeouts: TCP flows 1 hour (ipstack's 1-minute default would cut idle push channels), UDP
  flows 60 seconds, DNS flows 10 seconds.
- Rust API (Phase 0): `mobile_engine::start(OwnedFd, &str, Arc<dyn EventListener>) ->
  Result<EngineHandle>`, `EngineHandle::stop()` and `EngineHandle::traffic()`. `start` blocks while
  the DNS servers are set up. `stop` ends every flow and waits until the TUN descriptor is closed
  before shutting the runtime down: ipstack's TCP streams block until their tasks end when dropped,
  which never happens in a runtime that is shutting down, and the descriptor leaked that way.
- FFI through **UniFFI** (`src/ffi.rs`, proc macros, `uniffi.toml` sets the Kotlin package
  `dev.fanchao.cpxy.vpn.engine`), generating both the Kotlin and the Swift bindings:
  - `startEngine(tunFd: Int, configJson: String): Engine`, throwing
    `EngineException.Failed(reason)`
  - `Engine.stop()`, `Engine.traffic(): TrafficStats`
  - `Engine.connectionsSince(since, limit)` and `Engine.connectionsBefore(before?, limit)`: pages
    of the last 10,000 connections (`src/connection_log.rs`), each a `ConnectionEvent` mirroring
    `OutboundEvent` (host, port, `direct`/`proxy`, delay, time, optional error, country) with a
    `seq` that is never reused in the process. A page's `oldestSeq` shows which were dropped. The
    traffic screen polls `connectionsSince` every 500 ms while it follows the newest, and pages
    back with `connectionsBefore`.
- `uniffi-bindgen` is a binary of the crate behind the `bindgen` feature. It reads the bindings from
  a host debug build: release libraries are stripped and carry no UniFFI metadata.
- On Android the engine logs to logcat with the tag `cpxy-engine` (`paranoid-android`).
- Config (JSON, defined once in Rust with serde and mirrored in Kotlin):
  `server` (cpxy URL with key), `dns_upstream: [ServerSpec]`, `dns_alternative: [ServerSpec]`,
  optional `mtu`. `ServerSpec` is the existing `client::dns_split::spec` format (IP, `udp://`,
  `tcp://`, `tls://`, `https://`, `?ip=`).
- TUN parameters (set by the platform, mirrored in the engine): address `10.233.0.1/30`, DNS server
  `10.233.0.2` (virtual, answered by the engine), routes `0.0.0.0/0` and `::/0`, MTU 1500.
- Keep the tokio runtime small: 2 workers, since the iOS extension has a ~50 MB memory limit.
  It must be multi-threaded: ipstack's TCP streams block in place when dropped.
- The engine resolves the cpxy server hostname with the system resolver, which works because the
  engine is outside the tunnel. TUN flows arrive as IPs, so no extra resolver is needed for routing.
- Private DNS (Android DoT): reject TCP 853 so Android falls back to plain DNS, which the engine
  answers.

### KMP app: `mobile/`

- Own Gradle build (separate from `client/android-app`): `shared` (Compose Multiplatform UI,
  profiles in a preferences DataStore, and a `VpnController` interface that each platform
  implements; an interface rather than `expect`, so the UI takes it as a parameter), `androidApp`,
  and `iosApp/` (Xcode project with the app target and a PacketTunnel extension target).
- Screens: two bottom navigation tabs. "VPN": connect/disconnect, status, traffic and the profile
  list/edit (server URL, upstream and alternative DNS lists). "Traffic": a log of the connections
  made while it is shown, with their outbound and the country flag of the destination; it follows
  the newest unless the user scrolls away, caps itself at about 5 MB, and is dropped on leaving the
  tab, when the engine also stops reporting connections.
- Android: `CpxyVpnService` (`VpnService`) as a foreground service (type `specialUse`, subtype
  `vpn`) with a notification and stop action; hands `ParcelFileDescriptor.detachFd()` to the
  engine. Engine calls run one at a time off the main thread. `AndroidVpnController` asks for the
  VPN consent through the activity and starts the service. Gradle builds `libmobile_engine.so` with
  cargo-ndk (NDK `28.2.13676358`, cargo-ndk `4.1.2`, the same pins as the existing app) and
  generates the UniFFI Kotlin bindings, both as declared task inputs/outputs under `build/`.
- iOS: the extension passes the utun fd (from `packetFlow` via the usual KVC lookup) to the engine;
  fall back to bridging `readPackets`/`writePackets` if that breaks. Config reaches the extension
  through `NETunnelProviderProtocol.providerConfiguration`.

## Phases

### Phase 0: engine, tested on Linux (done)

- `mobile-engine` crate: ipstack loop, TCP → outbound, DNS hook, UDP 443 drop, UDP relay, IPv6
  reject.
- Unit tests for the routing decisions.
- A Linux integration test: open a real TUN in a network namespace, run the engine against a local
  `server` binary, and check CN-direct, proxied TCP, DNS split, QUIC refusal and direct UDP. Model it
  on `packaging/openwrt/test/lab.sh`. This is the main correctness gate and needs no phone.

Done: `mobile-engine/`, with unit tests for the config, the packet filter and the routing decision,
and `mobile-engine/test/tun-lab.sh`. The lab runs `mobile-engine-linux` (creates a TUN device and
runs the engine on it) and the release `server`:

```sh
cargo build --release -p server -p mobile-engine
mkdir -p bins && cp target/release/server bins/cpxy-server && cp target/release/mobile-engine-linux bins/
mobile-engine/test/tun-lab.sh bins
```

Besides the checks above it covers DNS over TCP, TCP 853 and IPv6 refusals, 16 parallel 4 MB
downloads, fail-closed when the server dies, and a clean stop. It runs in CI on pull requests, main and releases (Phase 2).

### Phase 1: Android MVP (done)

- `mobile/` Gradle project, shared UI, `CpxyVpnService`, UniFFI bindings, cargo-ndk build for
  `arm64-v8a`, `armeabi-v7a`, `x86`, `x86_64`.
- Manual smoke check on a device: connect, browse a CN and a non-CN site, check the public IP,
  disconnect.

Done: `mobile/` (see [mobile/README.md](../mobile/README.md)), with JVM tests for the profile
store, the engine config JSON and validation (`./gradlew :shared:allTests`). Debug and release
APKs carry `libmobile_engine.so` for all four ABIs.

The smoke check ran on an API 36 x86_64 emulator against a local `server`, with the consent
granted by `appops set dev.fanchao.cpxy.vpn ACTIVATE_VPN allow`: connect; the DNS split answered;
non-CN traffic (e.g. `ifconfig.me`) went through the server; CN addresses (223.5.5.5,
119.29.29.29) went direct and never reached it; disconnect from the app and from the notification
removed `tun0` and the foreground service, also with a flow open. The run found the descriptor
leak fixed in `stop` above. Still to do on a physical phone: a CN and a non-CN site in a browser
with the real server, and the public IP.

### Phase 2: CI for the APK (done)

New jobs in `.github/workflows/ci.yml`; existing jobs unchanged.

- Pull requests: `cargo test -p mobile-engine`, the namespace TUN integration test, and
  `./gradlew :shared:allTests` in `mobile/`.
- main and releases: `./gradlew :androidApp:assembleRelease`, check that
  `lib/*/libmobile_engine.so` is in the APK for all four ABIs, upload the APK as an artifact, and add
  the job to `release-upload`'s `needs`. The job needs the NDK, cargo-ndk and the four Rust
  targets; generating the bindings also builds the engine for the host.
- Signing: start with the checked-in debug keystore (`mobile/debug.keystore`, already used by the
  release build); move to a release keystore from GitHub secrets later.

Done: `.github/workflows/ci.yml` now has `test_mobile_engine` (unit tests and the real-TUN
namespace lab), `test_mobile_shared` (`:shared:allTests`) and `build_mobile_vpn` (release APK on
main and releases, after both test jobs pass). The APK job installs the four Rust targets, pinned
NDK and cargo-ndk, checks the packaged engine library for all four ABIs and uploads
`mobile-vpn-release.apk`. The distinct asset filename keeps it separate from the existing app's
`androidApp-release.apk`. `release-upload` depends on the new build and test jobs. Desktop
executable builds use `--workspace --exclude mobile-engine`: the engine uses Unix TUN descriptors
and must not be compiled by the Windows release jobs. It is built by its dedicated mobile jobs.

Validation: actionlint accepted the workflow; all 15 engine tests and 9 shared JVM tests passed;
the release Linux TUN lab passed (peak engine RSS 11 MB); `:androidApp:assembleRelease` passed
and the exact CI inspection confirmed all four engine libraries in the APK. The inspection also
rejected a fixture missing an ABI. Signing still uses the checked-in debug keystore; switching to
a release key in GitHub secrets remains later work.

### Phase 3: iOS

- xcframework build of `mobile-engine` (`aarch64-apple-ios`, plus simulator slices for the app UI),
  Swift bindings, `PacketTunnelProvider`, KMP `iosMain` controller.
- CI on `macos-latest`: build the xcframework and an unsigned `xcodebuild` of the app and
  extension. Network extensions do not run on the simulator.
- Device testing via TestFlight once a paid Apple Developer account exists (signing secrets in CI,
  App Store Connect upload).

### Phase 4: polish

- Android always-on VPN and a quick-settings tile.
- Per-app bypass list.

## Risks to watch

1. iOS extension memory limit (~50 MB): the lab measured a peak RSS of 12 MB on Linux (release
   build, 16 parallel downloads) and fails above 50 MB. Re-measure on iOS.
2. UDP relay: NAT table size and idle timeouts; load-test with a video call.
3. QUIC fallback speed: resolved. UDP 443 gets an ICMP port-unreachable reply, written to the TUN
   by the engine's packet filter, so browsers fall back at once.
4. Android Private DNS bypassing the virtual DNS server (mitigated by rejecting TCP 853).
