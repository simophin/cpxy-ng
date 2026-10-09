# CPXY VPN app

The Android (and later iOS) VPN app that runs the Rust `mobile-engine` on the device. See
[docs/mobile-vpn-plan.md](../docs/mobile-vpn-plan.md) for what it does and how.

Run Gradle from this directory and Cargo commands from the repository root.

- `shared/`: Compose Multiplatform UI, profile storage and the `VpnController` interface.
- `androidApp/`: `CpxyVpnService` (the `VpnService` running the engine), `AndroidVpnController`
  and the activity. Application ID `dev.fanchao.cpxy.vpn`, so it installs next to the existing
  app in `client/android-app`.

## Build and test

```bash
./gradlew :shared:allTests
./gradlew :androidApp:assembleDebug
./gradlew :androidApp:assembleRelease
```

The release variant uses the checked-in debug keystore, like the existing app.

The APK tasks build `libmobile_engine.so` for the four Android ABIs with cargo-ndk, and generate
the UniFFI Kotlin bindings from a host debug build of the engine (release libraries are stripped
of the UniFFI metadata). Both are declared task outputs under `androidApp/build/generated/`.
Install the toolchain once:

```bash
rustup target add \
  aarch64-linux-android \
  armv7-linux-androideabi \
  x86_64-linux-android \
  i686-linux-android

cargo install cargo-ndk --version 4.1.2 --locked
sdkmanager "ndk;28.2.13676358"
```

## CI and release APK

The repository CI runs the engine unit tests, the Linux namespace TUN lab and
`:shared:allTests` on pull requests, main and releases. On main and releases it also builds the
release APK with the pinned NDK and cargo-ndk, then checks that `libmobile_engine.so` is packaged
for `arm64-v8a`, `armeabi-v7a`, `x86` and `x86_64`.

Download the `mobile-vpn-release.apk` artifact from the CI run, or the asset with the same name
from a GitHub release. The distinct filename keeps it separate from the existing Android app.
Release APKs currently use the checked-in debug signing key.

## Manual smoke check

1. Install the debug APK, add a profile (server URL with key, upstream and alternative DNS) and
   connect; accept the VPN and notification prompts.
2. Browse a CN site and a non-CN site. The home screen lists each connection as `direct` or
   `proxy`.
3. Check the public IP (e.g. `https://ifconfig.me`): it is the cpxy server's.
4. Disconnect from the app or the notification.

The engine logs to logcat with the tag `cpxy-engine`.
