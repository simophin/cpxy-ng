# Android RTP v2 integration: agent implementation plan

Status: implementation and available automated/emulator checks complete. Remaining
acceptance limits and delivery are tracked below and in
[rtp-mobile-v2-validation.md](rtp-mobile-v2-validation.md).
Latest main `6e4ce52` was merged as `cb918e8`; the Tokio worker and packet channels
are retained. Actual starting commit: `77e80db` (plan on top of `60d4d9a`). The user explicitly
requested continuing on `docs/android-rtp-v2-plan`, overriding the suggested new
branch name below. Intended PR base: `main`, confirmed by `origin/HEAD`.
Baseline reviewed: `60d4d9a` (PR #153, HTTP-controlled RTP v2 and OpenWrt router).

## Objective and authorized delivery

Implement HTTP-controlled RTP v2 in the existing Android RTP app, application ID
`dev.fanchao.cpxy.rtp`, in `mobile-rtp/`. Work on a dedicated branch, make
reviewable commits, push the branch and open a **draft** pull request. Complete
the implementation and available checks before opening the draft. Do not merge,
publish a release, or deploy/change a public endpoint or live router.

This is the Android follow-up to #153. Its explicit Android exclusion applied
to that earlier work; this task authorizes the integration described here.
The protocol contract remains [rtp-http-protocol.md](rtp-http-protocol.md).
Read that contract and current code before editing; older mobile-v1 notes in
[rtp-mobile-plan.md](rtp-mobile-plan.md) are historical where they conflict.

## Branch setup and working rules

1. Read applicable `AGENTS.md` instructions and inspect `git status`, current
   branch, remotes and recent history. Preserve unrelated user changes.
2. Start a branch named `feat/android-rtp-v2`, or a unique suffixed variant if
   that name already exists. Use the current checkout containing #153 as the
   starting point; do not reset, overwrite another branch or pull unrelated work.
3. Record the actual baseline and intended PR base branch. Confirm the remote's
   default branch from repository metadata rather than assuming its name.
4. Implement the phases below with focused commits. Update this checklist and
   validation notes as work completes. Do not mark unrun checks as passed.
5. Continue autonomously through implementation, local verification, commits,
   branch push and draft PR creation within this scope. If credentials, network
   access or device availability prevent a step, finish unaffected work and
   report the precise limitation. Do not silently omit delivery steps.

## Reuse and scope boundaries

| Component | Action |
| --- | --- |
| `rtp-l3/src/v2/{codec,http,http_client,client_runtime,state}.rs` | Reuse the wire protocol, allocation, activation, rotation, repair and retry logic. |
| `rtp-mobile/src/lib.rs` | Adapt existing `prepare`, `Prepared::address`, `Prepared::start` and platform socket protection for Android lifecycle requirements. |
| `rtp-mobile/src/connect.rs` | Retain resolver ordering, family interleaving, authenticated UDP readiness and the two-live-allocation limit. |
| `rtp-mobile/src/{shim,packet,dns}.rs` | Reuse packet pump, DNS injection, routing assumptions, counters and refusals. |
| `rtp-mobile/src/ffi.rs` | Replace deferred-start rejection with an owned preparation/start API. |
| `mobile-rtp/shared/` | Migrate profiles and update the editor and tests. |
| `mobile-rtp/androidApp/` | Adapt service startup, durable identity, cancellation, recovery and resource ownership. |
| `.github/workflows/ci.yml` | Retain Rust/JVM and four-ABI APK checks; add focused checks where needed. |

Keep the separate proxy app in `mobile/` and `mobile-engine/` behavior unchanged.
Do not copy the OpenWrt firewall, shell routing, DNS listener or procd lifecycle
into Android. Keep Android's current regional/private direct routing, per-app
selection and self-exclusion. Inner traffic remains IPv4 at MTU 1200; retain
IPv6 capture/refusal, suppressed AAAA answers, QUIC refusal and tunneled
alternative DNS. Outer IPv4 and IPv6 remain supported.

No iOS implementation, v1 fallback, new transport protocol, per-client
credentials, updater, traffic-log feature or broad UI redesign is required.

## Phase 1: profiles, identity and compatibility

- [x] Remove manual inner address from the profile editor, validation and engine
  JSON. Explain that the endpoint assigns the tunnel address automatically.
- [x] Define endpoint as `host:port` of the **HTTP TCP control listener**. It is
  not a fixed UDP port. Preserve hostname and bracketed IPv6 support.
- [x] Add `server_identity`, default `cpxy-rtp`, to the shared Rust profile and
  Kotlin profile/editor. Validate the protocol's nonempty UTF-8, 255-byte bound.
  Pass it into `Client::new` instead of hardcoding it in `prepare`. Existing
  Linux/OpenWrt profiles must continue to parse using the default.
- [x] Persist a non-nil random client UUID per Android profile in app-private
  durable storage. Generate once and durably save **before** any allocation;
  concurrent connect attempts must not provision two identities for one profile.
  Do not generate a new UUID on every retry or process restart.
- [x] Keep identity separate from runtime keys/counters. Ordinary profile edits,
  including endpoint edits, preserve identity. A newly created profile gets a
  new identity; any profile-copy flow must also get a new identity.
- [x] Migrate old profiles without losing selected profile, name, endpoint, DNS
  lists or the encrypted derived key. Ignore/discard legacy `address`; preserve
  the existing profile ID used to index `AndroidKeyVault`.
- [x] Explicitly malformed persisted identity must surface an actionable error;
  do not silently replace it and allocate a second permanent server address.
  Missing identity on a legacy profile is the expected migration case.
- [x] Inspect Android backup configuration. Keep durable UUID state out of
  cross-device restore so restored/copied app data cannot accidentally clone an
  active identity. Document backup/restore behavior and lost-key behavior.
- [x] Reuse PSK derivation and Android Keystore wrapping. No password re-entry
  is required solely for v2 when a usable encrypted key already exists.
  Never persist allocation secrets, transmit counters or replay state.

Use the existing DataStore/repository patterns where practical. A UUID is an
identity, not a credential. Avoid a second independently mutable source of truth.

## Phase 2: native preparation API and ownership

The existing Rust engine already prepares an authenticated UDP path and learns
the assigned address before starting its packet worker. Expose that sequence
through UniFFI; do not reimplement HTTP or UDP control in Kotlin.

Required API behavior (names may follow repository conventions):

1. Create a preparation operation using validated profile JSON, a 32-byte PSK,
   persisted UUID and `RtpPlatform` callback.
2. Run preparation off the main thread. Obtain the assigned IPv4 address only
   after HTTP allocation and authenticated bidirectional UDP path validation.
3. Let Android establish the TUN using that address and the existing routes.
4. Consume the prepared session exactly once to start the packet engine using
   the transferred TUN descriptor. Preserve the prepared socket, allocation,
   transmit counters and receive state through that transfer.
5. Return the existing engine status/control surface. Report connected only
   after matching activation acknowledgement, not merely HTTP success or READY.

- [x] Provide cancellation accessible while preparation is in progress. The
  public shared `prepare()` currently has no caller cancellation handle;
  candidate-race cancellation alone is insufficient for Android Disconnect.
- [x] Propagate cancellation to candidate attempts and HTTP I/O where practical.
  Check cancellation around resolution and every ownership transfer. System
  resolution may need a bounded worker strategy; document any unavoidable
  resolver cancellation limit. A late result must never establish a VPN.
- [x] Keep preparation/race/release cleanup off the UI thread. `PreparedPath`
  currently performs a control release in `Drop`; account for its blocking I/O.
- [x] Ensure losing candidates, cancelled preparations and failed TUN startup
  attempt allocation release and close sockets. Release is best effort under
  network loss, with server expiry as the fallback; never wait indefinitely.
- [x] Reject invalid key length/UUID/profile before opening transport sockets.
  Make use-after-consume, repeated cancel and repeated stop deterministic.
- [x] Specify TUN descriptor ownership on every success/failure path, including
  repeated start on a consumed preparation. Close every accepted descriptor
  exactly once. Validate before constructing `OwnedFd` where appropriate.
- [x] Adapt the platform callback to `crate::Platform`; false protection results
  must abort the socket attempt. Protect every HTTP TCP socket and initial,
  rotating and repairing UDP socket before use.
- [x] Return bounded, useful error categories for configuration, identity,
  authentication/control rejection, transient connectivity and cancellation.
  Do not expose keys, plaintext envelopes or arbitrary OS/input details.
- [x] Restore/verify one-time Android log initialization without secret logging.
- [x] Remove deferred-start code and the validator's legacy `address` stripping
  shim once Kotlin migration supplies the current profile format.

Preserve existing Linux caller behavior through small wrappers/defaults if shared
signatures change. Avoid a parallel Android transport implementation.

## Phase 3: Android service lifecycle and fail-closed recovery

Implement this normal startup sequence:

```text
Foreground notification and Starting state
  -> load/migrate profile, key and durable UUID
  -> prepare protected HTTP/UDP connection off main thread
  -> obtain assigned inner IPv4
  -> establish TUN with assigned /32, DNS, routes and app selection
  -> transfer TUN descriptor to prepared engine
  -> activate path and monitor status
```

- [x] Serialize connect, reconnect, profile switch, revoke and stop transitions.
  Cancel an older operation before replacing it. Use an operation generation or
  equivalent guard so late results cannot overwrite a newer service state.
- [x] Keep cancellation callable while blocking preparation executes; the
  current single-lane engine dispatcher must not prevent Disconnect from
  signalling native cancellation. Coroutine cancellation alone does not stop
  a synchronous native function.
- [x] Preserve VPN permission handling, foreground-service timing, notification
  disconnect action, sticky restart and per-app routing semantics.
- [x] Track best non-VPN network availability, loss and return. Account for a
  network change during preparation, the preparation-to-engine handoff, and
  temporary loss followed by the same network returning. Serialize callback
  state with service operations; avoid callbacks into a closed engine.
- [ ] Retain `setUnderlyingNetworks` and explicit socket protection. Verify
  actual outer routing during Wi-Fi/cellular changes before adding stronger
  network-binding mechanisms.
- [x] Use runtime same-lease repair, allocation recreation, server-generation
  recovery and rotation first. Recreate the entire engine only when necessary.
- [x] Retry transient initial network failures with bounded backoff; the current
  service incorrectly treats every first-start failure as a profile problem.
  Surface permanent configuration/credential problems without a tight retry loop.
- [x] Preserve the TUN and captured routes during recoverable outages and
  fatal-worker restart. The current engine owns/closes its TUN and the service
  calls `stopEngine()` before retry, despite a comment claiming routes remain.
  Resolve this ownership gap explicitly.

Recommended TUN ownership approach: the service retains a
`ParcelFileDescriptor` and passes a detached duplicate to each worker. The
service-held reference keeps the interface installed while a failed worker is
stopped and its replacement prepares. Verify the Android interface actually
remains installed and traffic cannot escape during this interval. Do not let
stale shim tasks continue reading the same TUN when starting a replacement.

If the endpoint returns a different inner address after a server-state change,
rebuild the VPN interface under Android's replacement semantics while retaining
the previous interface until the replacement is established. Validate the
transition; do not assume descriptor duplication proves absence of a routing
gap. If the platform cannot meet this requirement, document the specific limit
and do not claim seamless fail-closed recovery.

- [x] Close service-held and engine-held descriptors on explicit Disconnect,
  permission revocation and service destruction. Stop retries and preparation,
  remove foreground notification, and ensure later completion cannot reconnect.
- [x] Clear temporary key byte arrays when ownership permits. Retain no key in
  service state longer than needed for the chosen restart strategy.
- [x] Preserve status counters and existing UI states, with useful sanitized
  messages for restart, outage and permanent errors. Show assigned address if
  useful without turning it back into editable configuration.

Fail-closed here covers an active service and its recovery transitions. Do not
claim protection after process death without Android always-on lockdown; that
feature is outside this task. Initial preparation before any VPN exists also
does not establish a system-wide traffic block.

## Phase 4: verification

Add focused behavioral tests for new lifecycle and migration contracts; reuse
existing codec and Linux coverage rather than duplicating the protocol suite.

### Automated checks

- [x] Rust: validate key/UUID/profile/server identity; preparation cancellation,
  failed protection, exactly-once consumption and descriptor cleanup; late race
  results release leases and never activate; existing Linux callers still work.
- [x] Kotlin shared: legacy JSON migration, old key/profile ID association,
  selection/DNS preservation, stable persisted UUID, fresh UUID for a new copy,
  malformed identity handling, automatic address and server-identity validation.
- [ ] Android service tests at the appropriate available test layer: stop during
  blocked preparation, rapid connect/stop/connect, profile replacement, network
  loss/return, failed establishment, worker failure and retained-TUN restart.
- [x] Run repository formatting and diff checks for modified files.

Commands, from the repository root unless noted:

```bash
cargo fmt --all -- --check
cargo test --locked -p rtp-l3 -p rtp-service -p port-admission -p rtp-mobile
cargo build --locked -p rtp-service -p rtp-mobile --bins
git diff --check
```

From `mobile-rtp/`:

```bash
./gradlew :shared:allTests
./gradlew :androidApp:assembleDebug :androidApp:assembleRelease
```

Run Android instrumentation tests if added and an emulator/device is available.
Verify regenerated UniFFI bindings compile and the APK contains
`librtp_mobile.so` for `arm64-v8a`, `armeabi-v7a`, `x86`, and `x86_64`.
Use existing pinned NDK/cargo-ndk versions and declared Gradle task outputs.
Do not hand-edit generated bindings. Run Clippy if available/applicable; report
missing toolchains or unrelated baseline failures accurately.

### Integration acceptance

- [x] Run `rtp-mobile/tests/linux-lab.sh` in an isolated network/mount namespace
  using its existing fixture procedure. Include outer IPv4/IPv6 and blocked-family
  endpoint-racing variants when changing shared connection code.
- [x] Use a disposable local v2 endpoint and Android emulator/device where
  available. Do not mutate a public endpoint to obtain a green result.
- [ ] Verify server-assigned address matches the VPN address; two independent
  profiles/devices get distinct identities and process restart preserves identity.
- [x] Confirm tunnelled TCP/UDP egress, direct regional/private traffic, both DNS
  paths, no AAAA leak, IPv6/QUIC refusal and all per-app selection modes.
- [x] Make HTTP reachable while one family's UDP is blackholed; verify the
  working candidate wins without reporting the unusable path connected.
- [ ] Exercise Wi-Fi loss/return and Wi-Fi/cellular handover when hardware allows;
  test changes during preparation as well as an established session.
- [x] Observe at least one real five-minute rotation with sustained traffic,
  acknowledged handover and old-allocation cleanup. Do not shorten production
  timers merely to make this check pass.
- [ ] Freeze/stop/restart the endpoint and force a worker restart. Observe
  captured routes and attempted app traffic throughout recovery; verify no
  tunneled destination goes direct. Verify direct traffic still works as designed.
- [ ] Disconnect during DNS resolution, HTTP preparation, UDP readiness,
  handoff and active repair. Revoke permission and switch profiles. Confirm no
  stale engine, repeated connection, owned socket or TUN remains after cleanup.
- [ ] Check existing installation upgrade with a usable encrypted key, missing
  key and corrupted identity. Messages must describe the actual next action.

Record commands, environment/API/ABI, observed outcomes, cleanup and limitations
in `docs/rtp-mobile-v2-validation.md`. Distinguish Linux, emulator and physical
device results. Hardware-only cellular, battery/screen-off and long-duration
checks may remain explicit follow-ups if unavailable; a draft PR can disclose
them. Build success alone is not Android transport validation.

### Completion notes

The final native suite passed 84 tests and shared JVM coverage passed 19 tests.
The Android default suite passed 11 tests; Settings-driven revocation and a real
five-minute rotation passed separately. Both APK variants contain all four ABIs,
and all four Linux family/racing variants passed after merging main. Clippy and
scoped formatting/diff checks completed; workspace formatting has unrelated
baseline differences documented in the validation report.

Unchecked acceptance items retain their full original scope: physical handover,
actual old-APK/process-restart upgrade validation, all cancellation/handoff timing
injections, forced TUN establishment failure, and conclusive absence of a routing
gap remain unverified. The available tests cover substantial portions of these
items without claiming their remaining portions. Permanent reconnect failures now
retain an existing TUN until explicit Disconnect, rather than silently removing
capture. See the validation report for the direct-control limitation and cleanup.

## Commit and documentation sequence

1. `feat(rtp-mobile): migrate Android profiles to v2 identity and addressing`
   — persistence/migration, automatic address, configurable server identity and
   focused profile tests. Keep the Android build coherent at this boundary.
2. `feat(rtp-mobile): expose cancellable v2 preparation through UniFFI`
   — native preparation ownership, platform adapter, cancellation/error surface
   and native behavioral tests. Update callers affected by API changes together.
3. `feat(android): connect RTP v2 with retained VPN recovery`
   — service startup/handoff, network callbacks, retry/stop, retained-TUN
   ownership and Android lifecycle coverage.
4. `test(rtp-mobile): validate Android v2 lifecycle and document migration`
   — integration fixtures/CI adjustments where necessary, validation report and
   updated `mobile-rtp/README.md` / `docs/rtp-mobile-plan.md`.

Adjust boundaries if needed to keep each commit buildable and reviewable. Stage
explicit paths, inspect staged diffs, and never commit secrets, local SDK files,
generated libraries/APKs, device captures containing credentials or fixture state.
Preserve historical #153 scope/results; append links to the follow-up instead
of rewriting history as though Android was validated there.

## Draft PR delivery and completion criteria

- [x] Inspect final diff and confirm scope, clean task-owned working state and
  completed available checks. Document material limitations and unfinished gates.
- [x] Push the implementation branch without force-pushing another person's work.
- [x] Update the existing draft PR using `gh` (#154).
  Suggested title: **Enable HTTP-controlled RTP v2 in the Android VPN app**.
  Describe the old deferred startup, resulting allocation-before-TUN behavior,
  identity/profile migration, cancellation/fail-closed ownership changes and
  concrete verification. Link #153 and this plan; do not claim checks not run.
- [x] For CLI PR creation/update, write the body to a temporary file and pass
  `--body-file`; preserve real newlines. Verify the returned PR is a draft and
  targets the intended base. Do not request review or merge automatically.
- [x] Report branch, commit hashes, draft PR URL, checks/results and outstanding
  device/environment limitations to the user. If push/PR creation is blocked,
  report exactly what succeeded and the remaining access requirement.

The implementation is review-ready when a migrated profile can obtain its
server-assigned address and connect through the shared v2 runtime, cancellation
cannot resurrect a stopped connection, recovery preserves captured routes,
existing shared/Linux behavior remains intact, and Android build/test evidence
and any unverified acceptance cases are explicit in the draft PR.

Delivery: pushed `docs/android-rtp-v2-plan`; implementation/validation checkpoints
`f40938e` and `3fdceaf` follow merge `cb918e8`. Updated existing
[draft PR #154](https://github.com/simophin/cpxy-ng/pull/154), confirmed draft
status and `main` base. Remaining acceptance limits are recorded in the validation
report and PR description.
