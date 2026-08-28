# Android and Desktop client development

Run Gradle from this directory and Cargo commands from the repository root.

## Build and test

Run the local Kotlin tests and build the existing variants with:

```bash
./gradlew :androidApp:testDebugUnitTest
./gradlew :androidApp:assembleDebug
./gradlew :androidApp:assembleRelease
```

The release variant currently uses the checked-in debug keystore. It is suitable
for a local or CI build, not a production release.

The Rust workspace tests are the current native-core baseline:

```bash
cargo test --workspace
```

## Desktop distributions

Build and exercise the current host's packaged application with:

```bash
./gradlew :shared:allTests :desktopApp:test \
  :desktopApp:desktopNativeSmoke :desktopApp:packagedNativeProbe \
  :desktopApp:packageDistributionForCurrentOS
```

CI runs that gate on Linux x64, Windows x64, macOS x64, and macOS arm64. Each
job inspects the packaged native library with host tools and uploads an unsigned
workflow artifact. The artifacts are not production releases and are not
signed or notarized.

## Android native libraries

Android APK/AAB tasks build and package the Rust client library automatically.
The build uses Android NDK `28.2.13676358` and cargo-ndk `4.1.2`; install them and
the four Rust targets once:

```bash
rustup target add \
  aarch64-linux-android \
  armv7-linux-androideabi \
  x86_64-linux-android \
  i686-linux-android

cargo install cargo-ndk --version 4.1.2 --locked
sdkmanager "ndk;28.2.13676358"
```

`./gradlew :androidApp:assembleDebug` now invokes `cargo ndk` as a declared
Gradle task input. The four generated libraries live exclusively under
`androidApp/build/generated/rustJniLibs`; no generated native binary belongs in
`src`.

## Manual smoke check

Install and open the debug APK, grant notification permission when requested,
create a profile, start it, open the event viewer, then stop it. Starting a
profile is the point that exercises loading the generated Rust library.
