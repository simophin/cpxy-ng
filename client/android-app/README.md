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
