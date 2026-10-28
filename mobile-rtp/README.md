# CPXY RTP app

The Android VPN app for an RTP internet-exit endpoint (`cpxy-rtp endpoint --internet-exit`). It
runs the shared HTTP-controlled RTP v2 runtime through the Rust `rtp-mobile` engine. See
[docs/rtp-mobile-plan.md](../docs/rtp-mobile-plan.md) for what it does and how.

Run Gradle from this directory and Cargo commands from the repository root. This build is
independent of the proxy app in `mobile/`.

- `shared/`: Compose Multiplatform UI, profile and settings storage, the `VpnController` and
  `Platform` interfaces. Only the Android target is built; the UI is in `commonMain`.
- `androidApp/`: `RtpVpnService` (the `VpnService` running the engine), `AndroidKeyVault` (the
  tunnel keys, encrypted with an Android Keystore key) and the activity. Application ID
  `dev.fanchao.cpxy.rtp`.

## Build and test

```bash
./gradlew :shared:allTests
./gradlew :androidApp:assembleDebug
./gradlew :androidApp:assembleRelease
```

Both variants are signed with the checked-in debug keystore. The APK tasks build
`librtp_mobile.so` for the four Android ABIs with cargo-ndk and generate the UniFFI Kotlin
bindings from a host debug build, as the proxy app does; the toolchain setup is the same (see
[mobile/README.md](../mobile/README.md)).

## Using it

1. Run a matching v2 endpoint with `--internet-exit`; note its HTTP TCP control
   `host:port`, shared password and server identity (default `cpxy-rtp`). Allocated
   UDP ports must also be reachable; the TCP control port is not a fixed UDP port.
2. Add a profile with that endpoint, password, server identity and DNS servers.
   Hostnames and bracketed IPv6 addresses are supported. The endpoint assigns
   the inner IPv4 address automatically after authenticated UDP readiness.
3. Connect. Addresses in the local region and private networks go direct; everything else goes
   through the tunnel. Under "Apps using the VPN", choose which apps use it.

The engine logs to logcat with the tag `cpxy-rtp`, and the service with `RtpVpnService`.

Existing profiles upgrade in place: names, selected profile, DNS settings and
Keystore-wrapped keys remain associated with the same profile IDs. The old manual
address is discarded. Each profile gets a durable random client UUID before its
first allocation; edits preserve it, and new profiles get new identities. A corrupt
UUID needs restoration or explicit profile recreation, rather than silently
allocating another permanent address. If the wrapping key is lost, enter the
password again; the UUID stays the same.

Backup and device transfer exclude the profile/identity and encrypted-key files.
Uninstalling or clearing app data creates fresh identities. Do not manually copy
private app data to another active device: copied UUIDs represent the same client.

Preparation and Disconnect cancellation run independently. During a recoverable
outage, the service keeps its TUN reference while the runtime repairs/recreates
allocations or a failed worker is replaced. Explicit Disconnect removes the VPN.
Initial preparation does not install a system-wide traffic block, and protection
after process death requires Android always-on lockdown (outside this app's scope).

For reproducible local endpoint/emulator checks and remaining device acceptance
limits, see [v2 validation](../docs/rtp-mobile-v2-validation.md) and the
[implementation checklist](../docs/rtp-mobile-v2-plan.md).
