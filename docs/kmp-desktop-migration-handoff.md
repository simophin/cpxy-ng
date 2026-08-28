# KMP + Desktop Migration Handoff

Status: implementation-ready proposal  
Last updated: 2026-08-28  
Primary scope: Android + JVM Desktop  
Future scope preserved: iOS through Kotlin/Native and the existing Rust C ABI

## 1. Purpose

This document is the implementation handoff for converting `client/android-app` from a single Android application module into a Kotlin Multiplatform application with shared Compose UI and application logic, while adding JVM Desktop support.

It is written for AI agents and human implementers. Treat the architectural decisions and sequencing below as the default unless repository evidence discovered during implementation requires a change. If a decision must change, update this document in the same change that alters the design.

The migration includes:

- A modern three-module KMP structure.
- Compose Multiplatform shared UI.
- Metro as the compile-time dependency injection framework.
- AndroidX DataStore Preferences as shared persistent storage.
- Compose Navigation 3 in shared UI.
- Gradle-owned Rust native-library builds for Android and Desktop.
- Desktop MSI and DMG installers plus a portable Linux application archive.
- Explicit lifecycle ownership and removal of `GlobalScope`.
- A future-compatible boundary for Kotlin/Native/iOS.

## 2. Executive decision record

The following decisions are already made.

| Area | Decision |
|---|---|
| Project structure | Keep `client/android-app` as the Gradle root for now; create `shared`, `androidApp`, and `desktopApp` modules inside it. |
| Shared UI | Use Compose Multiplatform in `shared/commonMain`. |
| Dependency injection | Use Metro `1.4.2`; use constructor injection by default and platform-specific final dependency graphs. |
| Persistent storage | Use AndroidX DataStore Preferences `1.2.1` in common code, storing the existing serialized JSON configuration as one preference value. |
| Navigation | Replace Navigation 2 with Navigation 3 `1.1.1` during the common UI move. |
| Rust/JVM interop | Retain the handwritten Rust C ABI and JNA for Android and JVM Desktop. |
| FFI generators | Do not adopt UniFFI in this iteration. Use cbindgen only to generate/check the C header. |
| Desktop runtime | Compose Desktop on JVM, packaged with `jpackage` through the Compose plugin. |
| Native artifacts | Cargo outputs are generated build inputs, never committed source files. |
| Lifecycle | Metro scopes cache dependencies but do not own cleanup. An explicit `AppLifecycle.close()` performs deterministic shutdown. |
| iOS | Not implemented in this iteration; preserve an interface suitable for a Rust `staticlib`/XCFramework and Kotlin/Native cinterop implementation. |
| Navigation state and DI | Compose owns the Navigation 3 back stack. Metro supplies controllers/repositories; it does not own UI navigation state. |

## 3. Goals and non-goals

### 3.1 Goals

1. Preserve current Android behavior throughout the migration.
2. Run the same profile/configuration/event UI on Android and Desktop.
3. Build Android APK/AAB artifacts and Desktop application images from a clean checkout using Gradle entry points.
4. Package the correct Rust library into each Android ABI and Desktop OS/architecture distribution.
5. Load the bundled native library explicitly and produce actionable diagnostics when it is absent or incompatible.
6. Make Kotlin application and native lifecycles deterministic and testable.
7. Preserve existing Android user configuration through a one-time migration.
8. Keep common code free of Android `Context`, JNA types, and JVM-only APIs.
9. Leave a credible future path to iOS without replacing the shared application API.

### 3.2 Non-goals

- Shipping an iOS application in this iteration.
- Replacing the Rust event WebSocket with FFI callbacks.
- Rewriting the proxy core.
- Adopting Navigation 3 alpha releases when a stable version is sufficient.
- Introducing feature-module navigation aggregation for the current two-screen application.
- Adopting Metro graph extensions before a real child-lifecycle use case exists.
- Redesigning the UI beyond changes required for Desktop usability.
- Solving release signing/notarization in the first functional Desktop milestone. Native placement must nevertheless be compatible with later signing.
- Renaming `client/android-app` during the functional migration.

## 4. Current repository inventory

### 4.1 Gradle and Kotlin

The current project has one Gradle module:

```text
client/android-app/
└── app/
    └── src/main/
```

Relevant files:

- `client/android-app/settings.gradle.kts` includes only `:app`.
- `client/android-app/app/build.gradle.kts` applies the Android application and Kotlin Android plugins.
- `client/android-app/gradle/libs.versions.toml` currently uses Kotlin `2.2.10`, AGP `8.13.0`, Ktor `2.3.8`, and JNA `5.17.0`.
- All Kotlin source is under `app/src/main/java`; there are no Kotlin/common/Desktop test source sets.

The current Kotlin `2.2.10` and AGP `8.13.0` combination is outside the published KMP compatibility matrix. Kotlin `2.2.10` is documented only through AGP `8.10.0`. Do not build the new structure on this mismatched lane.

### 4.2 Rust native boundary

`client/Cargo.toml` already declares:

```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

`client/src/dynlib.rs` exports two C ABI functions:

- `create_client(...) -> *mut c_void`
- `destroy_client(handle)`

The handle owns a Tokio runtime. Kotlin loads this library through JNA in `client/android-app/app/src/main/java/dev/fanchao/cpxy/Client.kt` and `App.kt`.

Android CI currently invokes `cargo ndk` and writes `.so` files directly to `app/src/main/jniLibs`. Gradle has no native build task or declared generated native input.

### 4.3 Portability blockers in Kotlin

The following current couplings must be removed before code can move to `commonMain`:

| File/area | Current coupling |
|---|---|
| `App.kt` | Android `Application`, `Context`, service locator, JNA loading, construction of all dependencies. |
| `ConfigRepository.kt` | Android `SharedPreferences` and AndroidX `edit`. |
| `ProfileInstanceManager.kt` | JNA `Pointer` in public state, Android logging, `GlobalScope`. |
| `EventsRepository.kt` | Android logging and `GlobalScope`. |
| `ProfileList.kt` | Resolves repositories/managers from `LocalContext.current.appInstance`; uses `java.util.UUID`. |
| `Settings.kt` | Resolves repository from Android context and shows Android `Toast`. |
| `EventViewer.kt` | Resolves repository from Android context; uses `java.time` and `java.text`. |
| `HomeScreen.kt` | Reads `R.string.app_name` through Android context. |
| `Theme.kt` | Android SDK checks and dynamic-color APIs. |
| `MainActivity.kt` | Owns the Navigation 2 graph. |
| service classes | Correctly Android-specific; must stay in `androidApp`. |

### 4.4 Existing native defects that must be fixed first

1. Rust declares `error_len` as `usize`; JNA maps it as `NativeLong`. This is ABI-incompatible on 64-bit Windows because Rust `usize` is 64-bit while Windows C `long` is 32-bit.
2. Kotlin uses `ptr.getInt(0) == 0` as a validity check. The pointer references an opaque Rust `Handle`, not an integer status value. Only nullness is a valid failure signal.
3. Truncated Rust error output is not guaranteed to be NUL-terminated.
4. No explicit panic containment exists at the FFI boundary.
5. There is no ABI version function, generated header, or automated cross-language smoke test.
6. Kotlin exposes a raw JNA `Pointer` in `ProfileInstanceManager.RunningState`, permitting accidental double-destroy/use-after-close patterns.

Do not begin Desktop loading against this ABI.

## 5. Target module and source-set structure

```text
client/android-app/
├── settings.gradle.kts
├── build.gradle.kts
├── gradle/
│   └── libs.versions.toml
├── shared/
│   ├── build.gradle.kts
│   └── src/
│       ├── commonMain/
│       │   ├── kotlin/dev/fanchao/cpxy/
│       │   │   ├── app/
│       │   │   ├── config/
│       │   │   ├── events/
│       │   │   ├── nativeclient/
│       │   │   ├── navigation/
│       │   │   └── ui/
│       │   └── composeResources/
│       │       ├── drawable/
│       │       └── values/
│       ├── commonTest/
│       ├── androidMain/
│       ├── androidHostTest/
│       ├── androidDeviceTest/
│       ├── jvmMain/
│       └── jvmTest/
├── androidApp/
│   ├── build.gradle.kts
│   └── src/
│       ├── main/
│       │   ├── AndroidManifest.xml
│       │   ├── kotlin/dev/fanchao/cpxy/android/
│       │   └── res/
│       ├── test/
│       └── androidTest/
└── desktopApp/
    ├── build.gradle.kts
    ├── resources/
    │   ├── common/
    │   ├── windows-x64/
    │   ├── linux-x64/
    │   ├── macos-x64/
    │   └── macos-arm64/
    └── src/
        ├── main/kotlin/dev/fanchao/cpxy/desktop/
        └── test/
```

Module responsibilities:

### `shared`

- Serializable models.
- Repository and state-machine logic.
- Common `NativeClient` API.
- Shared Compose screens and root UI.
- Navigation 3 routes and `NavDisplay`.
- DataStore-backed configuration repository.
- Metro-injectable common classes and common graph contract.
- Common tests and fakes.

### `androidApp`

- Android application entry point.
- Activity, foreground service, receiver, notification permission, manifest, launcher resources.
- Final Metro `AndroidAppGraph`.
- Android `Context` and platform storage providers.
- Android JNA AAR and `AndroidNativeClient`.
- Generated Android `jniLibs` wiring.
- One-time SharedPreferences migration source.
- Android logging/dynamic color/platform notification integration.

### `desktopApp`

- Desktop `main()` and Compose `application`/`Window` entry point.
- Final Metro `DesktopAppGraph`.
- Desktop app-data paths and DataStore storage provider.
- Desktop native-library resolution and `DesktopNativeClient`.
- Window-close/tray/exit behavior.
- Compose Desktop native distribution configuration.
- Generated native application-resource wiring.

## 6. Toolchain and dependency lane

Start with one tested compatibility lane. Do not independently select the newest version of every component.

| Component | Initial pin |
|---|---:|
| Kotlin / KMP plugin | `2.4.10` |
| Compose compiler plugin | `2.4.10` |
| Compose Multiplatform plugin | `1.12.0` |
| Metro | `1.4.2` |
| Android Gradle Plugin | `9.2.0` |
| Gradle wrapper | `9.4.1` |
| JVM bytecode target | `17` |
| Desktop runtime / packaging JDK | JetBrains JBRSDK `25`, provisioned through Foojay |
| Ktor | `3.5.2` |
| kotlinx.coroutines | `1.11.0` |
| kotlinx.serialization | `1.11.0` |
| AndroidX DataStore | `1.2.1` |
| Navigation 3 | `1.1.1` |
| JNA | `5.19.1` |

Important notes:

- The Compose compiler plugin version must match Kotlin.
- Compile both Android modules against API 37 while retaining Android application's `targetSdk = 36` and both modules' `minSdk = 26`. Compose Multiplatform 1.12 Android artifacts require API 37 at compile time; AGP 9.2 is the first stable AGP lane that supports API 37 and requires Gradle 9.4.1.
- Provision JetBrains JBRSDK 25 with the Foojay resolver and assign its home explicitly to Compose Desktop. Do not inherit the IDE's bundled runtime: it may omit `jpackage`. Keep emitted application bytecode at JVM 17 and pass `-Dawt.toolkit.name=auto` so JBR selects Wayland when available and X11 otherwise.
- AGP 9 has built-in Kotlin support for the Android application module. Follow the AGP 9 migration documentation rather than carrying the old `org.jetbrains.kotlin.android` configuration forward blindly.
- The `shared` module uses `org.jetbrains.kotlin.multiplatform` and `com.android.kotlin.multiplatform.library`.
- Explicitly pin Navigation 3 `1.1.1`. Compose Multiplatform `1.12.0` documents a Navigation 3 `1.2.0-alpha02` family, which is not the chosen baseline.
- Compose Multiplatform 1.12 component artifacts should be pinned in the version catalog. Inspect the resolved dependency graph for accidental alpha upgrades or mixed AndroidX/JetBrains artifact families.
- Metro is a Kotlin compiler plugin. Confirm its compatibility whenever Kotlin is changed.

Before source movement, make a dedicated toolchain change and run:

```bash
./gradlew projects
./gradlew help
./gradlew :androidApp:assembleDebug
```

The latter is available only after the module skeleton exists; keep toolchain and skeleton commits small enough to diagnose independently.

## 7. Metro dependency injection design

### 7.1 Rules

1. Use constructor `@Inject` for application-owned classes wherever possible.
2. Use `@Provides` for third-party objects, runtime inputs, dispatchers, and objects whose construction cannot be annotated.
3. Use `@ContributesBinding(AppScope::class)` for interface implementations that should be discovered across modules.
4. Scope process/application lifetime objects with `@SingleIn(AppScope::class)`.
5. Keep the common graph contract unannotated.
6. Define each final `@DependencyGraph` in platform-specific entry code. Metro requires this when common and platform-specific contributions are mixed.
7. Do not inject Android or JNA types into common constructors.
8. Do not use Metro to own Compose navigation state.
9. Do not assume graph destruction closes scoped resources. Call `AppLifecycle.close()` explicitly.
10. Avoid member injection unless framework-created Android objects require it; prefer retrieving graph-owned collaborators explicitly from the `Application` graph.

### 7.2 Common graph contract

Suggested shape in `shared/commonMain`:

```kotlin
@Scope
annotation class AppScope

interface AppGraph {
    val appController: AppController
    val appLifecycle: AppLifecycle
}

@Inject
@SingleIn(AppScope::class)
class AppController(
    val configRepository: ConfigRepository,
    val profileInstanceManager: ProfileInstanceManager,
    val eventsRepository: EventsRepository,
)
```

Expose the smallest useful root API. Do not expose every binding as a graph accessor merely for convenience; Metro treats accessors as graph roots.

### 7.3 Android graph

Define the final graph in `androidApp`:

```kotlin
@DependencyGraph(AppScope::class)
interface AndroidAppGraph : AppGraph {
    val clientServiceCoordinator: ClientServiceCoordinator

    @DependencyGraph.Factory
    fun interface Factory {
        fun create(
            @Provides applicationContext: Context,
        ): AndroidAppGraph
    }
}
```

Create it once from the Android `Application` using `createGraphFactory<AndroidAppGraph.Factory>()`. The `Application` may expose the graph to Android framework entry points, but shared composables receive `AppController` as an ordinary parameter rather than resolving it from `LocalContext`.

### 7.4 Desktop graph

Define the final graph in `desktopApp`:

```kotlin
@DependencyGraph(AppScope::class)
interface DesktopAppGraph : AppGraph {
    @DependencyGraph.Factory
    fun interface Factory {
        fun create(
            @Provides appPaths: AppPaths,
            @Provides nativeLibraryPath: NativeLibraryPath,
        ): DesktopAppGraph
    }
}
```

The Desktop `main()` resolves paths, creates the graph, passes `graph.appController` to the root composable, and guarantees `graph.appLifecycle.close()` on application exit.

### 7.5 Platform bindings

Common APIs:

```kotlin
interface NativeClient {
    fun start(config: NativeClientConfig): NativeClientSession
}

interface NativeClientSession : AutoCloseable {
    override fun close()
}

interface AppLogger {
    fun debug(tag: String, message: String)
    fun error(tag: String, message: String, throwable: Throwable? = null)
}
```

Platform implementations should use Metro contributions, for example:

```kotlin
@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class DesktopNativeClient(...) : NativeClient
```

Check exact Metro annotation placement during compilation; keep binding and scope intent as described even if syntax must be adjusted to the current API.

### 7.6 Lifecycle ownership

Provide an application coroutine scope as an AppScope singleton:

```kotlin
@Provides
@SingleIn(AppScope::class)
fun provideApplicationScope(): CoroutineScope =
    CoroutineScope(SupervisorJob() + Dispatchers.Default)
```

Do not cancel it before dependents have closed. Suggested shutdown order:

1. Stop the active `NativeClientSession`.
2. Stop event collection/reconnect loops.
3. Flush/close other owned resources where applicable.
4. Close Ktor `HttpClient`.
5. Cancel the application coroutine scope.

Make `AppLifecycle.close()` idempotent and test repeated calls.

## 8. DataStore design and migration

### 8.1 Supported model

Use these KMP artifacts from `shared/commonMain`:

```kotlin
implementation("androidx.datastore:datastore-core:1.2.1")
implementation("androidx.datastore:datastore-preferences-core:1.2.1")
```

Only DataStore Preferences is the supported KMP DataStore story for this migration. Do not introduce Proto DataStore.

DataStore is a data-layer KMP library, not a Compose-specific library. It exposes `Flow`; shared UI observes repository state through Compose state adapters.

### 8.2 Storage representation

Keep the existing configuration serialized as one JSON string:

```kotlin
val ConfigJsonKey = stringPreferencesKey("client_config_json")
val LegacyMigrationCompleteKey = booleanPreferencesKey("legacy_config_migrated")
```

Reasons:

- It closely matches the existing SharedPreferences representation.
- Profile lists remain one atomic document.
- Existing kotlinx.serialization defaults and `ignoreUnknownKeys` support schema evolution.
- It avoids flattening dynamic profiles into preference keys.
- It provides an easier future path to a database if configuration complexity grows.

### 8.3 Repository behavior

`ConfigRepository` belongs in `commonMain` and is constructor-injected:

```kotlin
@Inject
@SingleIn(AppScope::class)
class ConfigRepository(
    private val dataStore: DataStore<Preferences>,
    private val json: Json,
    private val applicationScope: CoroutineScope,
)
```

Requirements:

- Expose readiness explicitly. Do not make the native manager start from a default config and then restart when persisted config arrives.
- Prefer `StateFlow<ConfigLoadState>` or a configuration `Flow` that distinguishes loading, loaded, and corrupt-data/error states.
- Use `DataStore.edit`/`updateData` for all writes.
- Validate ports and profile data before persistence.
- Preserve unknown JSON fields when feasible; at minimum, continue `ignoreUnknownKeys` to support forward compatibility.
- Surface corrupt configuration as recoverable application state rather than crashing graph creation.

### 8.4 Platform storage

Android:

- Place the DataStore file beneath `Context.filesDir`.
- Provide the singleton `DataStore<Preferences>` from `AndroidAppGraph` bindings.
- Never create multiple DataStore instances for the same file.

Desktop:

- Place the file in the OS application-data directory, not a temporary directory.
- Default locations:
  - Windows: `%APPDATA%/Cpxy/`
  - macOS: `~/Library/Application Support/Cpxy/`
  - Linux: `$XDG_CONFIG_HOME/cpxy/`, falling back to `~/.config/cpxy/`
- Create directories before opening storage.
- Keep path computation in a testable `AppPaths` class.

Future iOS:

- Use Application Support or Documents storage with Okio, following the AndroidX KMP DataStore guidance.

### 8.5 Android legacy migration

Current data is stored in `getSharedPreferences("default", MODE_PRIVATE)` under key `config`.

Implement a one-time importer:

1. Open DataStore.
2. If `client_config_json` already exists, never overwrite it.
3. If the migration-complete flag is false and DataStore has no config, read legacy SharedPreferences key `config`.
4. Decode/validate the legacy JSON before copying it.
5. In one DataStore transaction, write the JSON and mark migration complete.
6. If no legacy value exists, mark migration complete and use the normal default configuration.
7. Do not delete the old SharedPreferences value in the first release. It provides rollback safety.
8. Test valid, missing, malformed, already-migrated, and DataStore-already-populated cases.

Hide Android access behind a small optional common abstraction or perform migration in an Android-provided initializer. `ConfigRepository` itself must not import Android APIs.

## 9. Navigation 3 design

### 9.1 Version and ownership

Use:

```toml
navigation3 = "1.1.1"
navigation3-ui = "org.jetbrains.androidx.navigation3:navigation3-ui:1.1.1"
```

The shared root composable owns the Navigation 3 back stack. Do not put it in Metro `AppScope`.

### 9.2 Routes

Define a sealed serializable route hierarchy in `shared/commonMain`:

```kotlin
@Serializable
sealed interface AppRoute : NavKey

@Serializable
data object HomeRoute : AppRoute

@Serializable
data class EditProfileRoute(
    val profileId: String?,
) : AppRoute
```

Use explicit polymorphic serialization configuration for `NavKey` so the design remains valid when iOS is added. Do not rely on JVM-only behavior.

### 9.3 Root UI

Conceptual shape:

```kotlin
@Composable
fun CpxyApp(appController: AppController) {
    val backStack = rememberNavBackStack(HomeRoute)

    NavDisplay(
        backStack = backStack,
        onBack = { backStack.removeLastOrNull() },
        entryProvider = entryProvider {
            entry<HomeRoute> {
                HomeScreen(
                    controller = appController,
                    editProfile = { backStack += EditProfileRoute(it.id) },
                    createProfile = { backStack += EditProfileRoute(null) },
                )
            }

            entry<EditProfileRoute> { route ->
                EditProfileScreen(
                    profileId = route.profileId,
                    controller = appController,
                    onDone = { backStack.removeLastOrNull() },
                )
            }
        },
    )
}
```

Treat the snippet as architectural pseudocode. Adjust to the exact stable `1.1.1` API after compiling against it.

### 9.4 Navigation tests

Test:

- Initial route is Home.
- Create profile opens `EditProfileRoute(null)`.
- Edit profile passes the correct ID.
- Done/back returns Home.
- Back at root does not corrupt the stack.
- Back-stack serialization/restoration handles both route types.
- Desktop Escape/back behavior is acceptable.

Do not add Metro navigation multibindings while there are only two routes. Reconsider them only if independently compiled feature modules need to contribute destinations.

## 10. Shared UI migration rules

### 10.1 Dependency access

Remove every `LocalContext.current.appInstance` lookup. Shared screens receive state and actions or an injected root controller passed from `CpxyApp`.

Preferred progression:

1. Extract a stateless/private rendering composable if one does not already exist.
2. Move repository collection and event handling to a screen-level controller/presenter or the shared screen wrapper.
3. Pass immutable state and action callbacks into rendering UI.
4. Keep previews/tests independent of Metro and platform contexts.

### 10.2 Resources

Move shared strings and non-launcher images to `commonMain/composeResources`.

- Replace `context.getString(R.string.app_name)` with Compose resource access.
- Keep Android adaptive launcher icons and notification icons in `androidApp/src/main/res`.
- Add `.ico`, `.icns`, and `.png` Desktop distribution icons later in the packaging phase.

### 10.3 UUID and time

- Replace `java.util.UUID.randomUUID()` with `kotlin.uuid.Uuid.random()`.
- Keep event timestamps as a common instant type.
- Isolate locale-sensitive date/time/number formatting behind a common formatter interface if the chosen common library cannot express current output.
- Do not move `java.time` or `java.text` imports into `commonMain`.

### 10.4 Theme

- Keep common light/dark color schemes and typography in shared UI.
- Treat Android dynamic color as an optional platform enhancement.
- One viable design is for the Android entry point to calculate an optional scheme and pass it to the common theme; Desktop passes null and uses the common scheme.
- Do not introduce `expect`/`actual` solely for a cosmetic feature if parameter injection is simpler.

### 10.5 User-visible errors

- Replace Android Toast calls in shared workflows with snackbar/dialog event state.
- Logging and user notification are separate concerns.
- Native load/ABI errors should be explicit and actionable, including expected path, OS, architecture, and underlying loader error.

## 11. Rust ABI hardening

Complete this before Desktop integration.

### 11.1 Target ABI

Prefer namespaced, fixed-width exports:

```c
#define CPXY_CLIENT_ABI_VERSION 1

typedef void* cpxy_client_handle;

uint32_t cpxy_client_abi_version(void);

cpxy_client_handle cpxy_client_create(
    uint16_t http_proxy_port,
    uint16_t socks5_proxy_port,
    uint16_t api_proxy_port,
    const char* dns_server,
    const char* main_server_url,
    const char* ai_server_url,
    const char* tailscale_server_url,
    char* error_buffer,
    uint32_t error_buffer_capacity
);

void cpxy_client_destroy(cpxy_client_handle handle);
```

Renaming the existing functions is acceptable because the only known consumer is this repository. If compatibility is desired, temporarily export forwarding aliases, but do not preserve the broken length type.

### 11.2 Safety requirements

- Validate every pointer before dereferencing.
- Required strings must reject null with a useful error.
- Optional strings must accept null.
- Always NUL-terminate the error buffer when capacity is greater than zero.
- Never write beyond capacity.
- Return null on all creation failures.
- Catch panics at the FFI boundary or establish an explicit abort policy; a panic must never unwind into JNA/Kotlin.
- Treat the handle as opaque on the Kotlin side.
- Destroy must accept null as a no-op.
- Kotlin session close must be thread-safe/idempotent enough to prevent double destroy.
- Add an ABI-version check before calling create.

### 11.3 Header generation

- Add cbindgen configuration and a reproducible header-generation command.
- Check the generated header into an appropriate interface/reference location if that improves reviewability, or generate and compare it in CI.
- CI must fail when Rust exports and the expected header diverge.

### 11.4 ABI tests

Rust tests:

- Version function returns the declared ABI version.
- Invalid required pointers produce failure without panic.
- Invalid URL and DNS inputs return terminated error text.
- Zero-length and one-byte error buffers are safe.
- Destroy null is safe.

JVM/JNA tests on each Desktop OS:

- Load by absolute path.
- Check ABI version.
- Decode an error from an invalid configuration.
- Start and close a valid session.
- Repeat close safely through the Kotlin owner.
- Report a clear missing-library error.

## 12. Native build and packaging design

### 12.1 General requirements

- Pin Rust with `rust-toolchain.toml`.
- Pin cargo-ndk and Android NDK versions.
- Cargo invocation must declare inputs/outputs sufficiently for Gradle up-to-date behavior.
- Generated binaries go under module `build/` directories.
- Never write generated native binaries to `src/`.
- Gradle package/run tasks depend on the relevant native build/copy tasks.
- CI must start from a clean checkout and must not rely on preinstalled developer artifacts.

### 12.2 Android

Target ABIs:

| Android ABI | Rust target |
|---|---|
| `arm64-v8a` | `aarch64-linux-android` |
| `armeabi-v7a` | `armv7-linux-androideabi` |
| `x86_64` | `x86_64-linux-android` |
| `x86` | `i686-linux-android` |

Suggested generated root:

```text
androidApp/build/generated/rustJniLibs/
├── arm64-v8a/libclient.so
├── armeabi-v7a/libclient.so
├── x86_64/libclient.so
└── x86/libclient.so
```

Register it as a generated `jniLibs` source directory and wire Cargo tasks to Android merge/package tasks using supported AGP APIs. Avoid brittle task-name string matching when a variant API is available.

Android JNA uses the AAR artifact. Keep existing R8/JNA rules and verify them after the module move.

Acceptance checks:

- APK/AAB inspection shows one native library for every intended ABI.
- Device/emulator instrumentation test loads the library and checks ABI version.
- A clean `assembleDebug` triggers native generation or gives a clear prerequisite error.

### 12.3 Desktop

Initial support matrix:

| OS | Architecture | Rust target | Library name |
|---|---|---|---|
| Windows | x64 | `x86_64-pc-windows-msvc` | `client.dll` |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `libclient.so` |
| macOS | x64 | `x86_64-apple-darwin` | `libclient.dylib` |
| macOS | arm64 | `aarch64-apple-darwin` | `libclient.dylib` |

Compose application resources root:

```text
desktopApp/build/generated/appResources/
├── windows-x64/client.dll
├── linux-x64/libclient.so
├── macos-x64/libclient.dylib
└── macos-arm64/libclient.dylib
```

Configure `appResourcesRootDir` to the generated root or a prepared root containing generated native files plus static resources.

Native loader resolution order:

1. An explicit development/test system property such as `cpxy.native.library.path`.
2. The packaged Compose property `compose.application.resources.dir` plus the expected native filename.
3. Fail with a detailed diagnostic.

Do not silently fall back to `PATH`, `LD_LIBRARY_PATH`, or a developer Cargo target directory in packaged applications; that can hide broken packaging.

Use `Native.load(absolutePath, ClientLibrary::class.java)`.

### 12.4 Platform-specific concerns

Linux:

- Build `x86_64-unknown-linux-gnu` against the oldest glibc baseline supported by the application, using a controlled container/runner.
- Do not load a musl `.so` into a glibc JVM distribution.
- Inspect with `ldd` and fail CI on unexpected dependencies.

Windows:

- Use x64 initially.
- Inspect with `dumpbin /DEPENDENTS` or an equivalent tool.
- Decide whether to statically link the MSVC CRT or include/install required runtime dependencies.

macOS:

- Package x64 and arm64 separately unless a deliberate universal strategy is adopted. The bundled JVM runtime is architecture-specific even if the Rust library is universal.
- Ensure the nested dylib is covered by signing.
- Later validate with `codesign --verify --deep --strict` before notarization.

### 12.5 GeoIP reproducibility

`geoip-data/build.rs` currently downloads GeoIP content during clean Cargo builds. Native target matrices amplify this network dependency.

Before considering native builds hermetic:

- Pin the remote content by checksum/version.
- Prefer a cached or vendored source artifact.
- Make offline builds possible after dependencies/data are prepared.
- Avoid each target job fetching mutable data independently.

Treat this as part of native build reliability, not an unrelated cleanup.

## 13. Desktop application behavior

The first Desktop version must define these behaviors explicitly:

- Window opens to the shared Home screen.
- Starting a profile runs the Rust proxy in-process.
- Running state and errors are visible in the window.
- Events continue through the existing loopback WebSocket.
- Stop closes the native session.
- Exit always closes the native session, Ktor client, and application scope.
- Persistence survives process restart.

Recommended first close policy:

- Closing the primary window exits the application and stops the proxy.
- Add tray/minimize-to-tray behavior only after basic lifecycle correctness is proven.
- If tray support is later enabled, retain an explicit Stop and Exit action because tray availability varies on Linux desktops.

Open product/security decision:

- Rust currently binds HTTP and SOCKS listeners to `0.0.0.0`. This exposes the proxy to the LAN and can trigger Desktop firewall prompts. Decide whether the Desktop default should instead bind loopback. Do not change behavior silently during structural commits.

## 14. Testing strategy

### 14.1 Common tests

- `ClientConfig` serialization/defaults/unknown fields.
- Profile create/update/delete/clone behavior.
- Legacy JSON validation.
- DataStore repository load/write/error behavior using test storage.
- Native manager state machine with a fake `NativeClient`.
- Native start failure and recovery.
- Reconfiguration closes the previous session exactly once.
- Event decoding for Connected and Error events.
- Event reconnect, retry, and cancellation.
- `AppLifecycle.close()` ordering and idempotence.
- Navigation route serialization and back-stack operations.

### 14.2 Android host/device tests

- SharedPreferences migration cases.
- Android DataStore file construction creates a singleton.
- Foreground service follows running state.
- Stop receiver disables the active profile.
- Native library load and ABI version on at least arm64 device/emulator.
- Release/R8 build retains required JNA interfaces.

### 14.3 Desktop tests

- App-data paths for Windows/macOS/Linux.
- Development native path resolution.
- Packaged resource path resolution.
- Unsupported OS/architecture diagnostic.
- Native library missing and ABI mismatch diagnostics.
- Start/stop and clean application shutdown.
- Configuration persistence across graph recreation.

### 14.4 Packaged smoke tests

For every CI host:

1. Build Rust library.
2. Build application image/installer.
3. Inspect the package for the native library.
4. Run the application image or a dedicated packaged native probe.
5. Load the bundled library by the same resolution path used in production.
6. Check ABI version and an error-path call.
7. Inspect dynamic dependencies.

UI automation is optional for the first package gate; native loading from the packaged layout is not optional.

## 15. CI design

Keep Rust unit tests, Android builds, and Desktop packages independently diagnosable.

Suggested jobs:

### `rust-test`

- Stable pinned toolchain.
- `cargo test`.
- FFI-specific tests.
- cbindgen/header consistency check.

### `android-app`

- Linux runner with pinned JDK and NDK.
- Install pinned cargo-ndk.
- Run common/JVM-appropriate tests.
- Generate all selected Android native ABIs.
- Assemble debug and release.
- Inspect packaged native entries.
- Run instrumentation native smoke test when a device/emulator lane is available.

### `desktop-linux-x64`

- Controlled glibc build environment.
- Common/JVM tests.
- Rust cdylib build.
- Compose application image and Linux package.
- Packaged native probe and `ldd` inspection.

### `desktop-windows-x64`

- Windows runner.
- Common/JVM tests.
- Rust MSVC cdylib build.
- Compose application image and MSI/EXE as selected.
- Packaged native probe and dependency inspection.

### `desktop-macos-arm64` and `desktop-macos-x64`

- Matching macOS runners or an explicitly supported cross-architecture strategy for the Rust artifact plus native JVM package.
- Compose DMG/PKG as selected.
- Packaged native probe and `otool -L` inspection.
- Signing/notarization may remain disabled until credentials/process are introduced.

Release upload must depend on all required target jobs.

## 16. Phased implementation plan

Each phase should leave the repository buildable. Prefer one focused PR or a small series of focused commits per phase.

### Phase 0: Baseline and guardrails

Work:

- Record current Android debug/release build commands and known runtime behavior.
- Add minimal tests for config serialization and current Rust core.
- Add/pin `rust-toolchain.toml` if absent.
- Document the current generated-native prerequisite until Gradle integration replaces it.

Acceptance gate:

- Existing Android application can still be built and manually smoke-tested.
- No migration code has changed behavior.

### Phase 1: Rust ABI hardening

Work:

- Replace platform-width length with fixed-width type.
- Remove opaque-pointer dereference from Kotlin.
- Namespace exports and add ABI version.
- Guarantee terminated errors and panic containment.
- Add cbindgen header and Rust/JNA ABI tests.
- Wrap handles in an idempotent Kotlin session owner.

Acceptance gate:

- Rust ABI tests pass.
- Host JNA error-path/version tests pass on Linux and Windows x64.
- Android native create/destroy behavior still works.

### Phase 2: Toolchain and module skeleton

Work:

- Move to the pinned Kotlin/Compose/AGP/Gradle/Metro lane.
- Create `shared`, `androidApp`, and `desktopApp` modules.
- Move the existing Android entry point/resources to `androidApp` without sharing UI yet if necessary.
- Configure the new Android-KMP library plugin in `shared`.
- Configure a minimal Desktop window.

Acceptance gate:

- Gradle sync succeeds.
- `shared` common and platform compilations succeed.
- Android app launches.
- Desktop hello/root window launches.

### Phase 3: Metro and lifecycle conversion

Work:

- Apply Metro to all relevant modules.
- Define `AppScope`, common `AppGraph`, final platform graphs, and factories.
- Constructor-inject repositories/managers/controllers.
- Replace the Android `Application` service locator as the source of shared dependencies.
- Introduce owned application scope and `AppLifecycle`.
- Remove all `GlobalScope` usage.

Acceptance gate:

- Metro validates both graphs at compile time.
- Android behavior is unchanged.
- Repeated lifecycle close tests pass.
- No shared constructor accepts Android context or JNA pointer.

### Phase 4: DataStore conversion

Work:

- Introduce KMP DataStore Preferences.
- Store config as one JSON value.
- Provide Android and Desktop storage paths through Metro.
- Add explicit repository readiness/error state.
- Implement and test one-time Android SharedPreferences migration.

Acceptance gate:

- Existing Android config survives upgrade.
- Desktop config survives restart.
- Manager does not start once with defaults before persisted config is ready.
- No `SharedPreferences` import remains in shared code.

### Phase 5: Shared Compose UI and Navigation 3

Work:

- Move models, event DTOs, repository logic, theme core, and screens into `commonMain`.
- Replace context service location with parameters/controllers.
- Move shared strings/assets to Compose resources.
- Replace UUID/time/JVM-specific usages.
- Introduce Navigation 3 routes and root `NavDisplay`.
- Remove Navigation 2 dependencies and graph.

Acceptance gate:

- Android and Desktop render the same core screens.
- Navigation tests pass.
- Common UI has no Android imports.
- Preview/render tests can construct screens without Metro or platform contexts.

### Phase 6: Gradle-owned Android Rust artifacts

Work:

- Add pinned cargo-ndk Gradle tasks.
- Generate `.so` files beneath `androidApp/build`.
- Register generated `jniLibs` and wire variants/tasks.
- Stop CI from writing native libraries into `src`.
- Inspect APK/AAB entries.

Acceptance gate:

- Clean Gradle Android build produces native artifacts and APK/AAB.
- Android instrumentation loads and checks ABI version.
- No generated `.so` exists under a source directory.

### Phase 7: Desktop native integration

Work:

- Add host Rust cdylib build/copy tasks.
- Implement explicit Desktop native path resolution.
- Contribute `DesktopNativeClient` through Metro.
- Implement Desktop app-data paths, application lifecycle, and initial close policy.
- Validate proxy start/events/stop manually and automatically where practical.

Acceptance gate:

- Desktop development run starts and stops Rust successfully.
- Events display.
- Missing/incorrect native library errors are actionable.
- Exit releases ports and terminates background work.

### Phase 8: Desktop packaging and CI matrix

Work:

- Configure Compose native distributions and OS/arch resources.
- Build Windows x64, Linux x64 glibc, macOS x64, and macOS arm64 artifacts.
- Add package inspection, native probe, and dependency inspection.
- Address GeoIP data reproducibility.

Acceptance gate:

- Every supported packaged application loads its own bundled native library on a clean machine/runner.
- Installers/application images are uploaded as CI artifacts.
- Dynamic dependency reports contain no unexplained missing runtime.

### Phase 9: Release hardening

Work:

- macOS signing/notarization.
- Windows signing.
- Android release signing cleanup; do not ship with the checked-in debug signing configuration.
- Desktop icons, metadata, upgrade identifiers, and version unification.
- Decide tray behavior and bind-address defaults.

Acceptance gate:

- Release artifacts meet platform installation/security expectations.

### Phase 10: iOS feasibility spike, not production implementation

Work:

- Add Rust `staticlib` output.
- Build iOS device/simulator slices.
- Generate header and assemble an XCFramework.
- Prove a Kotlin/Native cinterop call to `cpxy_client_abi_version` and an error-path create call.

Acceptance gate:

- The existing common `NativeClient` contract can be implemented without changes.
- Findings are documented before production iOS work begins.

## 17. Suggested commit/PR sequence

Keep changes reviewable and bisectable:

1. `rust: harden and version client C ABI`
2. `android: adapt JNA client to versioned opaque ABI`
3. `build: establish compatible KMP toolchain lane`
4. `build: split shared androidApp and desktopApp modules`
5. `app: introduce Metro graphs and owned lifecycle`
6. `config: migrate persistence to KMP DataStore`
7. `ui: move Compose screens and resources to commonMain`
8. `ui: migrate navigation to Navigation 3`
9. `build: generate Android Rust libraries through Gradle`
10. `desktop: add native client and application lifecycle`
11. `desktop: package OS-specific native distributions`
12. `ci: add host package matrix and native probes`

Combining adjacent steps is acceptable only when an intermediate state cannot compile. Avoid mixing Rust ABI changes, KMP source movement, and Desktop packaging in one unreviewable change.

## 18. Definition of done for the first iteration

The Android + Desktop migration is complete when all of the following are true:

- The Gradle project contains `shared`, `androidApp`, and `desktopApp` with the responsibilities defined above.
- Common UI, navigation, models, repositories, and application state compile from `commonMain`.
- Metro constructs and compile-time validates Android and Desktop graphs.
- No `GlobalScope` remains in the application.
- No shared code imports Android, JNA, `java.time`, or `java.text` APIs.
- DataStore Preferences stores configuration on Android and Desktop.
- Existing Android SharedPreferences config migrates safely.
- Navigation 3 `1.1.1` owns the shared back stack.
- The Rust ABI is fixed-width, versioned, header-described, panic-contained, and tested.
- Android native libraries are generated by Gradle under `build/` and packaged for intended ABIs.
- Desktop native libraries are generated/copied by Gradle and loaded from explicit packaged resources.
- Windows x64, Linux x64 glibc, macOS x64, and macOS arm64 packages are built in CI.
- Packaged native smoke tests prove the application loads its bundled library.
- Android remains functionally equivalent for profile management, proxy settings, start/stop, and events.
- Desktop supports configuration, start, events, stop, clean exit, and persistence restart.
- Known release-signing and product-policy follow-ups are explicitly tracked.

## 19. Agent execution rules

Agents implementing this plan must follow these constraints:

1. Inspect current files and `git status` before editing. Preserve unrelated user changes.
2. Keep the Android application ID `dev.fanchao.cpxy` unless explicitly instructed otherwise.
3. Do not commit generated `.so`, `.dll`, `.dylib`, APK, installer, or Gradle build output.
4. Do not expose JNA types through common APIs.
5. Do not create a second DataStore instance for the same file.
6. Do not put Navigation 3 back-stack state in Metro AppScope.
7. Do not use `GlobalScope` as a migration shortcut.
8. Do not rely on implicit native library search paths for packaged Desktop execution.
9. Do not silently upgrade to Navigation 3 alpha versions.
10. Do not delete legacy SharedPreferences until at least one release has proven migration and rollback behavior.
11. Preserve an independently testable fake for `NativeClient` so common tests never require Rust.
12. Run verification proportional to each phase and report commands that could not run.
13. Update this handoff when implementation evidence invalidates a decision.

## 20. Primary references

- Recommended KMP structure: <https://kotlinlang.org/docs/multiplatform/multiplatform-project-recommended-structure.html>
- Android-KMP library plugin: <https://developer.android.com/kotlin/multiplatform/plugin>
- KMP compatibility matrix: <https://kotlinlang.org/docs/multiplatform/multiplatform-compatibility-guide.html>
- Compose Multiplatform 1.12: <https://kotlinlang.org/docs/multiplatform/whats-new-compose-112.html>
- Compose Desktop native distributions: <https://kotlinlang.org/docs/multiplatform/compose-native-distribution.html>
- Metro 1.4.2 documentation: <https://zacsweers.github.io/metro/1.4.2/>
- Metro dependency graphs: <https://zacsweers.github.io/metro/1.4.2/dependency-graphs/>
- Metro multiplatform pattern: <https://zacsweers.github.io/metro/1.4.2/multiplatform/>
- Metro scopes: <https://zacsweers.github.io/metro/1.4.2/scopes/>
- Metro compiler compatibility: <https://zacsweers.github.io/metro/1.4.2/compatibility/>
- KMP DataStore: <https://developer.android.com/kotlin/multiplatform/datastore>
- Navigation 3 for Compose Multiplatform: <https://kotlinlang.org/docs/multiplatform/compose-navigation-3.html>
- AndroidX Navigation 3 releases: <https://developer.android.com/jetpack/androidx/releases/navigation3>
- Ktor client engines: <https://ktor.io/docs/client-engines.html>
- JNA loading: <https://github.com/java-native-access/jna/blob/master/www/GettingStarted.md>
- cargo-ndk: <https://github.com/bbqsrc/cargo-ndk>
- Rust linkage and `cdylib`: <https://doc.rust-lang.org/reference/linkage.html>
- Rust FFI guidance: <https://doc.rust-lang.org/nomicon/ffi.html>
- Kotlin/Native C interop: <https://kotlinlang.org/docs/native-c-interop.html>
