> HTTP-controlled RTP v2 migration: mobile transport integration is deferred.
> The shared engine now fails explicitly before runtime/socket setup. See
> [Linux v2 transport](../rtp-service/README.md). The implementation notes below
> describe the former mobile-v1 engine and are historical until adapted.

# CPXY RTP app

The Android VPN app for an RTP internet-exit endpoint (`cpxy-rtp endpoint --internet-exit`). It
runs the Rust `rtp-mobile` engine on the device. See
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

1. Run an endpoint with `--internet-exit`, and note its password and a `--client-address` for
   this device (the first is `10.88.0.2`).
2. Add a profile: the endpoint's `host:port`, the password, the tunnel address and the DNS
   servers. The app keeps only the key derived from the password.
3. Connect. Addresses in the local region and private networks go direct; everything else goes
   through the tunnel. Under "Apps using the VPN", choose which apps use it.

The engine logs to logcat with the tag `cpxy-rtp`, and the service with `RtpVpnService`.
