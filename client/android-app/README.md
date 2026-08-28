# Android client development

The Android client is currently a single `:app` Gradle module. Run Gradle from
this directory and Cargo commands from the repository root.

## Build and test

Run the local Kotlin tests and build the existing variants with:

```bash
./gradlew testDebugUnitTest
./gradlew assembleDebug
./gradlew assembleRelease
```

The release variant currently uses the checked-in debug keystore. It is suitable
for a local or CI build, not a production release.

The Rust workspace tests are the current native-core baseline:

```bash
cargo test --workspace
```

## Current native-library prerequisite

Gradle does not build the Rust library yet. Before installing an APK that needs
to start the proxy, generate the Android libraries from the repository root:

```bash
rustup target add \
  aarch64-linux-android \
  armv7-linux-androideabi \
  x86_64-linux-android \
  i686-linux-android

cargo install cargo-ndk

cargo ndk \
  -t aarch64-linux-android \
  -t armv7-linux-androideabi \
  -t x86_64-linux-android \
  -t i686-linux-android \
  -o client/android-app/app/src/main/jniLibs \
  build --release -p client --lib
```

This requires the Android SDK/NDK to be installed and discoverable by
`cargo-ndk`. The generated `jniLibs` directory is ignored by Git. A Gradle-only
build can produce an APK without these libraries, but starting a configured
profile then fails when JNA tries to load `client`.

## Manual smoke check

Install and open the debug APK, grant notification permission when requested,
create a profile, start it, open the event viewer, then stop it. Starting a
profile is the point that exercises loading the generated Rust library.
