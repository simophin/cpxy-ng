# KMP/Desktop migration progress handoff

Status date: 2026-08-29

Baseline commit: `093c2a4` (`Update run flags`)

Companion architecture plan: [`kmp-desktop-migration-handoff.md`](kmp-desktop-migration-handoff.md)

This document is the operational handoff for the migration. It records what the repository actually implements, why key choices were made, and the next commit-sized units another agent can execute without reconstructing prior decisions.

## Repository safety and working conventions

- Preserve the user's local IDE changes. At this handoff they are `.idea/deploymentTargetSelector.xml` (modified) and `.idea/ktfmt.xml` (staged) below `client/android-app`; neither belongs to migration commits.
- Keep each logical unit in its own compiling commit. Stage or commit only explicitly owned paths so the pre-staged IDE file is never swept into a commit.
- Native outputs belong below Gradle/Cargo build directories, never source directories.
- Use Gradle as the developer and CI entry point for both Kotlin and Rust orchestration.
- Do not add signing credentials, publish releases, or select product artwork without an explicit product/security decision.

## Decisions already made

1. **KMP structure:** shared state, lifecycle, persistence, and Compose UI live in `shared`; `androidApp` and `desktopApp` are thin platform launchers/providers.
2. **Version lane:** Gradle 9.4.1, AGP 9.2.0, Kotlin/Compose compiler 2.4.10, Compose Multiplatform 1.12.0, compile SDK 37, JVM bytecode 17, and Foojay-provisioned JetBrains JBRSDK 25 for Desktop runtime/packaging.
3. **Dependency injection and ownership:** Metro final graphs own an application `CoroutineScope`; `AppLifecycle` closes native sessions, HTTP resources, and the scope in a deterministic order. No `GlobalScope` or process singleton app instance.
4. **Persistence:** KMP DataStore Preferences is authoritative. Android performs a one-time, non-destructive migration from legacy SharedPreferences; Desktop uses conventional per-OS config paths.
5. **UI/navigation:** Compose screens, theme, and resources are common; Navigation 3 is pinned to stable 1.1.1.
6. **Native ABI:** the Rust C ABI is namespaced/versioned, uses fixed-width types and opaque handles, validates pointers, contains unwinds, and exposes an ABI version checked before use.
7. **Native build ownership:** Android and Desktop Gradle tasks invoke Cargo, place libraries under module `build/generated`, and wire them into assembly/run tasks. Source trees do not contain generated binaries.
8. **Desktop loading:** development uses an explicit absolute Gradle-provided native path; packaged applications resolve the native library from Compose application resources. JNA loads the absolute path and reports actionable platform/path/ABI errors.
9. **Packaging:** desktop packages are host-built; cross-packaging is not assumed. Signing/notarization and public release publication remain credential-gated follow-ups.

## Completed work

| Unit | Commit | Result |
| --- | --- | --- |
| Baseline guardrails | `37b4dc1` | Android config tests, pinned Rust toolchain, build/run documentation |
| Hardened Rust ABI | `a7c56a3` | Versioned opaque ABI, checked-in header, boundary/panic tests |
| Android ABI adapter | `d5d961c` | JNA client uses the versioned ABI |
| Compatible KMP build lane | `cd7c428` | Gradle/AGP/Kotlin/Compose/dependency pins and JVM 17 |
| Android module rename | `47ddd0a` | `app` became `androidApp`; CI/artifact paths updated |
| Shared and Desktop modules | `c433f18`, `f2b1ec0`, `f1404e3` | KMP skeleton and runnable Compose Desktop module |
| Application architecture | `2840e0a` | Metro graphs, owned scope/lifecycle, common repositories/contracts |
| KMP persistence | `ea71839`, `847c3cd`, `4edbbcb` | DataStore primitives/repository and Android legacy migration |
| Compose/API lane | `e1faf53`, `c09bb5d` | API 37 compatibility and common resources |
| Shared UI and navigation | `60d3947`, `514b49a` | Common screens/theme and Navigation 3 |
| Android native builds | `9a5781e` | Gradle-produced four-ABI Rust libraries packaged in APK |
| Desktop native runtime | `d3dd3ac` | Host Rust build, explicit resolver/JNA client, tests and native smoke task |
| Reproducible GeoIP input | `4fb9097` | Reviewed local derivative, checksum enforcement, no build-time download |
| Unsigned Desktop distributions | `5a51908` | Host package identity, unified version, verified image contents, packaged native probe |
| Desktop packaging JDK | `f16c79e` | Foojay-provisioned JBRSDK 25 and `jpackage` preflight |
| Host Desktop CI matrix | `40d4f0d` | Four host/architecture jobs, packaged probes, native/package inspection, unsigned artifacts |
| Windows installer identity | `desktop: stabilize Windows installer upgrades` (this commit) | Stable MSI upgrade UUID for in-place upgrades |

The last verified Desktop checks were `:desktopApp:test`, `:desktopApp:desktopNativeSmoke`, and `:desktopApp:packagedNativeProbe`. Both probes loaded ABI version 1 and exercised the Rust error path. A GUI window was not opened in the headless build environment.

## Phase 8A — reproducible GeoIP input

Status: **completed 2026-08-28**

### Problem

`geoip-data/build.rs` downloads a mutable jsDelivr URL on every clean native build. This makes IDE runs, offline builds, CI matrices, and release reconstruction depend on network availability and upstream content at build time.

### Implemented

1. Vendored the already-filtered CN IPv4 artifact at `geoip-data/data/cn-geoip.dat` rather than the much larger upstream database.
2. Recorded its upstream URL, retrieval date, transformation, SHA-256, and update procedure in `geoip-data/SOURCE.md`.
3. Replaced the downloader/decoder build script with a local read, pinned SHA-256 validation, and copy into `OUT_DIR`.
4. Removed the unused normal dependency and the reqwest, protobuf, filter, and IP-network build dependencies. `cargo tree -p geoip-data --locked --offline` now shows only `sha2` and its checksum primitives beneath the crate.

### Completion evidence

- Vendored and generated files both have SHA-256 `76997024829ff7b43948f781c69fd8aa90f4ba1e3d3e3f6b84363fb68a6c8ed1` and size 62,230 bytes.
- `cargo build -p client --lib --release --locked --offline` passed.
- `cargo test --workspace --locked --offline` passed, including 10 client tests, 10 core tests, and the server loopback test. The loopback test required normal host socket permissions.
- `./gradlew :desktopApp:desktopNativeSmoke` rebuilt the Rust library, loaded Desktop native ABI 1, and passed the native error-path probe.
- A second `./gradlew :desktopApp:buildDesktopRustLibrary` reported the native producer `UP-TO-DATE`.
- `git diff --check` passed.

## Phase 8B — unsigned Desktop distributions

Status: **completed 2026-08-28**

### Implemented

1. Configured host-built DMG and MSI formats with the stable `Cpxy` package name and `dev.fanchao.cpxy` macOS bundle ID.
2. Added `cpxy.version` as the single Android `versionName` and Desktop `packageVersion` source.
3. Made Compose application-resource preparation depend on the host Rust producer, so clean image and installer builds cannot consume a stale or missing native library.
4. Added `verifyDesktopApplicationImage`, which fails unless the application image contains exactly one native library with the current host's expected name.
5. Added a headless `--native-probe` path to the real Desktop launcher and a `packagedNativeProbe` Gradle task that builds, inspects, and executes the application image through the packaged-resource resolver.

### Completion evidence and host limits

- The full Gradle regression gate passed: shared tests/build, Android unit tests/debug APK, Desktop tests/compilation, the direct native smoke probe, and the packaged native probe.
- The Android assembly rebuilt and packaged all four supported Rust ABIs.
- The Linux application image contained exactly one native resource at `Cpxy/lib/app/resources/libclient.so`; the packaged launcher loaded ABI version 1 and exercised the Rust error path with exit code 0.
- `cargo test --workspace --locked` passed all workspace and documentation tests, including the server loopback test.
- `git diff --check` passed.

## Phase 8C — pinned Desktop packaging JDK

Status: **completed 2026-08-28**

### Implemented

1. Added Foojay toolchain resolution and selected JetBrains JBRSDK 25 explicitly for the Desktop module.
2. Assigned the resolved JBRSDK home to Compose Desktop, removing its dependency on whichever Gradle runtime the IDE or shell happens to use.
3. Added `verifyDesktopPackagingJdk`, which fails with a targeted diagnostic if the selected SDK does not contain `jpackage`.
4. Granted JNA native access explicitly for JDK 25.
5. Replaced the Linux DEB target with a versioned portable `.tar.gz` application archive and later enabled the native AppImage application-image task.

### Completion evidence

- Foojay provisioned JetBrains JBRSDK 25.0.4.1 and the preflight found its `bin/jpackage`.
- `:desktopApp:test` passed.
- `:desktopApp:packagedNativeProbe` rebuilt the application image with the pinned JBR, found exactly one `libclient.so`, loaded ABI version 1, and passed the native error path.
- The image's linked runtime contains only the requested modules and the complete application image is 206 MiB; JCEF was not copied into the runtime image.
- The earlier Arch OpenJDK launcher messages (`pure virtual method called` and `terminate called without an active exception`) were not reproduced with JBRSDK 25.
- `packageDistributionForCurrentOS` produced `Cpxy-1.0.1-linux-x64.tar.gz`; its extracted launcher retained executable permissions and passed `--native-probe`.

## Phase 9 — host CI release matrix

Status: **implemented 2026-08-29**

### Implemented

1. Added a fail-fast-disabled four-entry matrix for Linux x64, Windows x64, macOS x64, and macOS arm64. The macOS labels intentionally select `macos-15-intel` and the arm64 `macos-15` host rather than cross-compiling native packages.
2. Each host runs shared/Desktop Kotlin tests, the direct Rust ABI smoke probe, the packaged native probe, and `packageDistributionForCurrentOS` through Gradle.
3. Linux verification checks the application image and portable archive with `readelf`, `ldd`, and `tar`, including architecture, unresolved linkage, native-library uniqueness, and launcher permissions.
4. Windows verification administratively extracts the MSI and uses Visual Studio's host `dumpbin` on both image and MSI copies of `client.dll` to enforce x64 PE architecture and inspect dependencies.
5. macOS verification mounts the DMG and checks both image and DMG copies with `lipo` and `otool`, enforcing the matrix architecture and rejecting dependencies outside system or bundle-relative paths.
6. The matrix uploads only explicitly named unsigned workflow artifacts. It is intentionally not a dependency of the existing public release-upload job.

### Completion evidence and host limits

- On Linux x64, `:desktopApp:packagedNativeProbe` loaded ABI version 1 and passed the native error path from the packaged application image.
- The Linux inspector passed against `Cpxy-1.0.1-linux-x64.tar.gz`: one ELF64 x86-64 `libclient.so`, no unresolved `ldd` dependencies, one archived native library, and preserved `0755` launcher permissions.
- The workflow YAML, Bash syntax, and `git diff --check` passed locally.
- Windows and macOS packaging/inspection cannot run on the Linux development host. Their first authoritative evidence will be the matrix jobs; failures are kept independent with `fail-fast: false` so all host diagnostics remain available.

## Phase 9B — Windows installer upgrade identity

Status: **completed 2026-08-29**

### Implemented

1. Added a fixed Windows MSI upgrade UUID derived from the stable application identity, so later MSI versions participate in the same upgrade family.
2. Documented the upgrade contract next to the Desktop packaging instructions.
3. Left Android's existing debug-keystore release signing behavior unchanged.

### Completion evidence

- The existing signed Android release variant assembled and contained all four expected Rust ABI libraries.
- Shared/Desktop tests and compilation passed.
- The packaged Desktop native probe loaded ABI version 1 and passed the native error path.
- Gradle accepted the Compose Desktop 1.12 Windows packaging configuration, and `git diff --check` passed.

## Remaining units after Phase 9B

### Phase 9C — product/release decisions

- Obtain approved desktop icons and product metadata.
- Revisit Android production signing only if the release policy changes; the current debug-key behavior is intentionally retained.
- Decide macOS Developer ID/notarization and Windows Authenticode credentials.
- Confirm legal/license obligations for the repository and vendored GeoIP derivative.
- Decide service bind policy (loopback versus `0.0.0.0`), tray/background behavior, Linux glibc baseline, and Windows CRT policy.

### Phase 10 — iOS feasibility spike

- Treat iOS as a feasibility spike, not part of the first Desktop release.
- Inventory Rust/JNA/API assumptions that block Kotlin/Native and identify the smallest FFI/lifecycle prototype.
- Do not weaken the completed Android/Desktop architecture merely to imply unsupported iOS parity.

## Full regression gate

Run after packaging/CI changes, and before declaring the first migration iteration complete:

```text
./gradlew :shared:allTests :shared:build \
  :androidApp:testDebugUnitTest :androidApp:assembleDebug \
  :desktopApp:test :desktopApp:compileKotlin \
  :desktopApp:desktopNativeSmoke :desktopApp:packagedNativeProbe
cargo test --workspace --locked
```

Also run the host's package/image tasks and the packaged launcher probe described above. Record platform-specific skips explicitly; do not describe an unexecuted cross-platform package as verified.
