# Android RTP v2 validation

Implementation started at `77e80db` on the user-requested
`docs/android-rtp-v2-plan` branch, building on #153 (`60d4d9a`). Latest main
`6e4ce52` was merged as `cb918e8`; its Tokio client/endpoint and bounded packet
channels are retained. The Android recovery errors propagate through the async
worker, and the regression fixtures use its device pump.
[Draft PR #154](https://github.com/simophin/cpxy-ng/pull/154) targets `main`,
confirmed through GitHub repository metadata.

## Profile and ownership contracts

Profiles retain their IDs and encrypted-key association. Legacy manual addresses
are discarded. A missing client UUID is generated inside a DataStore transaction
that completes its durable write before allocation. Malformed explicit identities,
including null/non-string JSON, fail without provisioning a replacement. Edits
preserve identity; saving a new profile ID generates a new UUID even if a copied
object contains one. Closing and reopening storage preserves the UUID and selection.

The endpoint is the HTTP TCP control listener. `server_identity` defaults to
`cpxy-rtp`; Kotlin and Rust enforce its 1–255 byte UTF-8 limit without changing its
content. Existing Linux profiles retain the default. Password derivation and the
Keystore wrapping format/alias are unchanged. Keystore loss or corrupt ciphertext
asks for the password again while preserving the UUID.

UniFFI preparation validates configuration/key/UUID before socket use, returns an
address only after authenticated UDP readiness, and transfers the same path,
transmit counters and receive window into the worker once. `Connected` requires
ACTIVATED. A false protection callback aborts socket use. Every valid transferred
TUN descriptor is closed on failure, including repeated start; invalid descriptors
are rejected before constructing `OwnedFd`. A foreign finalizer moves prepared-path
cleanup to a worker thread.

Cancellation is independent of the blocking engine lane. Endpoint and DoT/DoH
hostname lookups share a pool of at most two detached resolver workers, a five-second
caller deadline, and 20 ms cancellation checks. DNS addresses are resolved before
allocation/TUN handoff while retaining the hostname for TLS SNI. libc getaddrinfo
itself cannot be interrupted; late results contain addresses only. Candidate races
check cancellation every 20 ms; the synchronous UDP handshake has at most 50 ms
read waits. Cancellation shuts down in-flight HTTP and prevents new control sockets.
An already-created lease may therefore expire on the server instead of completing
best-effort release. HTTP attempts have finite deadlines; runtime secrets/counters
are never persisted.

The service retains a TUN descriptor and transfers a duplicate to each worker.
It stops old shim tasks before a replacement reads that TUN. Disconnect, revoke and destroy close both references. Permanent initial errors
end startup; a permanent reconnect error retains an existing TUN without a retry
loop until the user reconnects or disconnects. A changed server assignment
makes the async worker return `AddrNotAvailable`, then preparation obtains the new
address and establishes a replacement before the service closes its old reference.
Generation guards and main-thread final installation reject stale starts. Stop
completion is published after the guarded `stopSelfResult`, avoiding a stale stop
racing an immediate Connect. Runtime repair/recreation/rotation precede whole-engine
restart, and transient initial failures retry with bounded backoff.

Backup is disabled; explicit cloud/transfer exclusions cover profile files and
key preferences. Manual copying of private app data is outside this guarantee.
Clearing app data/uninstalling creates fresh identities; the server retains old
permanent address mappings. Initial preparation does not block all system traffic.
Protection after process death requires Android always-on lockdown, outside scope.

## Environment and automated checks after merging main

- Linux x86_64 host; Rust 1.97.1, Java 21, cargo-ndk 4.1.2, NDK 28.2.13676358.
- Emulator `cpxy-sni-test`: Android 16 / API 36, x86_64, Google Play image.
- `cargo test --locked -p rtp-l3 -p rtp-service -p port-admission -p rtp-mobile`:
  **84 passed**, one existing production-timer test ignored by default.
- `cargo build --locked -p rtp-service -p rtp-mobile --bins`: passed.
- `./gradlew :shared:allTests`: **19 passed**, including storage reopen, concurrent
  UUID provisioning, corrupt identity shapes, ID/selection/DNS preservation and
  UTF-8 identity boundaries.
- `:androidApp:assembleDebug :androidApp:assembleRelease
  :androidApp:assembleDebugAndroidTest`: passed; regenerated bindings compile.
  Both APKs contain `librtp_mobile.so` for arm64-v8a, armeabi-v7a, x86 and x86_64.
- Clippy passed with existing warnings in unchanged code/dependencies. Formatting
  passes for `rtp-mobile`, `rtp-l3`, `rtp-service`; `git diff --check` passes.
  Workspace-wide fmt still reports pre-existing differences in
  `client/src/outbound/{http,protocol}.rs` and
  `cpxy-ng/src/{encrypt_stream,ws_stream}.rs`; those files were preserved.
- CI now builds both RTP APK variants and compiles lifecycle instrumentation on
  PRs, retaining the existing pinned four-ABI packaging check. It does not run
  the emulator fixture automatically or publish the RTP APK.

## Linux transport integration

The final binaries passed `rtp-mobile/tests/linux-lab.sh` in isolated
user/network/mount namespaces for `RTP_LAB_OUTER=4`, `6`, `block4`, and `block6`.
This checks tunneled TCP/UDP, direct regional traffic, split DNS, suppressed AAAA,
QUIC/DoT refusal, changed outer address repair and frozen-endpoint fail-closed
behavior. Blocked-family cases keep HTTP reachable on both families and require
UDP readiness before choosing the working family. These are Linux results;
Android outer IPv6 and physical cellular handover are not inferred from them.

## Android integration

The post-merge default suite passed **11 tests**, with its two opt-in checks
skipped. It covers:

- Assigned VPN address matching tunneled TCP source, independent profiles getting
  different UUIDs/addresses, profile switching and reconnecting to the original UUID.
- Tunneled UDP, direct private traffic, upstream and alternative DNS, empty AAAA,
  QUIC refusal, IPv6 connection failure and all three per-app selection modes.
- Cancellation during blocked native HTTP, transient initial retry, repeated rapid
  Connect/Stop/Connect and complete explicit-stop cleanup.
- Exactly-once native preparation consumption, repeated stop, accepted-FD cleanup
  and survival of the service-style retained descriptor.
- Permanent reconnect rejection retains capture until explicit Disconnect, while
  intended direct traffic continues.
- Endpoint freeze/resume, restart, changed-assignment fatal worker restart, and
  keeping an installed TUN while the replacement prepares.
- Emulator network disable/return with the TUN retained and traffic recovering.

The separate permission test passed after selecting **CPXY RTP → Forget VPN →
Forget** in Android Settings. It observed Disconnected, no TUN, and no reconnect
after a further two seconds. Changing appops alone does not revoke an already
active VPN. The emulator initially had a System UI ANR/stale Settings view; the
successful run used a refreshed Settings process and granted notification permission.

The separate production rotation passed on the merged async runtime with 64
successful TCP probes over more than five minutes. Port 33008 was active; 33012
was prepared at observation elapsed 308.81 s and 33008 was retired at 326.38 s.
The initial allocation appeared at 15.03 s, so this follows the unchanged
five-minute activation schedule and drain. Sustained data continued after handover.

Scoped host packet observation saw 162 encrypted RTP datagrams and no direct TCP
to the reserved test destination during the 116.51 s recovery/network-return window.
That observation alone is not a conclusive routing-gap oracle: the emulator refused
the direct positive-control connection even with a temporary host loopback listener.
The listener/address were removed and cleanup verified. No seamless fail-closed
guarantee across every platform transition is claimed; interface retention,
worker replacement and eventual tunneled egress were observed directly.

## Reproduce the local checks

The main fixture starts a real endpoint, HTTP/UDP echoes and DNS responders in a
private network/mount namespace. Host relays bind loopback only. Its fixed password
is disposable test data. UDP relays normalize the endpoint-visible source tuple,
so this fixture does not prove physical Wi-Fi/cellular roaming. Socket readiness
and channel I/O are nonblocking, including stale readiness after ICMP errors.
Restart preserves the current state directory; reassignment uses a fresh directory
and another address pool. Recovery control timestamps are in `events.jsonl`.

```bash
cargo build --locked -p rtp-service -p rtp-mobile --bins
sudo python3 mobile-rtp/tests/local-endpoint.py /tmp/rtp-android-fixture
# In another terminal, boot a disposable emulator, then from mobile-rtp/:
./gradlew :androidApp:connectedDebugAndroidTest
```

The emulator uses HTTP control `10.0.2.2:33000`, UDP allocation ports 33001–33016,
and direct fixture DNS on 33053. Instrumentation grants VPN consent using appops,
launches the app, and runs packet probes as shell UID because the app itself is
excluded from capture. The production-timer check takes more than five minutes:

```bash
./gradlew :androidApp:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=dev.fanchao.cpxy.rtp.RtpLifecycleTest#productionRotation \
  -Pandroid.testInstrumentationRunnerArguments.rotation=true
```

For Settings-driven revocation, select the permission test with
`-Pandroid.testInstrumentationRunnerArguments.manualRevoke=true`. Once logcat says
`Ready for VPN settings permission revocation`, open Android VPN Settings and forget
CPXY RTP. The test waits up to three minutes for that interaction.

## Remaining acceptance limits

Physical Wi-Fi/cellular handover, Android outer IPv6, battery/screen-off behavior,
long-duration use and an actual old-APK upgrade were not exercised. The migration
and wrapped-key compatibility have code/unit coverage; that is distinct from a
real installation upgrade. Cancellation at every DNS/readiness/handoff/repair
boundary and forced `Builder.establish()` failure are not all instrumented. Resolver
cancellation limits and best-effort release/expiry behavior are described above.
No protection after process death without always-on lockdown is claimed. Recovery
observations apply to this emulator and fixture, not every Android device.


## Cleanup and delivery

Disposable endpoint/relay processes and private namespaces are stopped after the
checks. The temporary direct-control loopback address was removed; the host
loopback list was checked afterward. Test-created profiles/keys are deleted by
instrumentation, which uninstalls its APKs. Captures and fixture state are local
validation artifacts and are not committed. No public endpoint or live router was
changed, and no release was published. The unrelated untracked directories remain
outside the implementation commits.

Pushed implementation/validation checkpoints `f40938e` and `3fdceaf` on
`docs/android-rtp-v2-plan`, then updated draft PR #154 with the final scope,
checks and acceptance limits. The PR remains a draft targeting `main`.

## CI follow-up: gateway approval reporting

The initial PR CI failure was `Test cpxy-tsgw`: `TestThroughput` timed out waiting
for the gateway to report exit-node approval. The same failure also occurred on
main, while the RTP/Android checks passed. A fresh fetch confirmed `6e4ce52` was
still latest main and already merged; `git merge origin/main` was up to date.

Approval reporting now reads `Device::self_node()` directly instead of building a
full status snapshot through the peer/path actors. This removes unrelated peer
queries from the approval log's five-second deadline. The harness retains its
approval assertion and now also checks approval before a peer joins, withdrawal
and restoration with an active peer, and traffic after restoration. Failed tests
include control-server diagnostics; CI enables targeted gateway/control logging.

The release binary built, Go formatting/vet and targeted Rust formatting passed.
With CI's Go 1.27.1, the expanded packet-path and Go-state tests passed three
consecutive runs (six test executions). Both tests also passed with 10 ms network
latency in a disposable namespace. The original intermittent failure was not
reproduced locally, including repeated runs with constrained scheduling; the
change removes the unnecessary status-query dependency without weakening checks.
