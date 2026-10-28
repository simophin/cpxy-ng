# Android RTP v2 validation

Implementation baseline: `77e80db`, building on #153 (`60d4d9a`). Work continues
on the user-requested `docs/android-rtp-v2-plan`; intended draft PR base is `main`
(`origin/HEAD`). This report is updated as checks run. Unchecked plan gates are
not claimed as validated.

## Implemented contracts

Profiles retain their existing IDs and encrypted-key association. Legacy manual
addresses are discarded. A missing client UUID is generated inside a DataStore
transaction that finishes before native allocation; malformed explicit UUIDs
fail without replacement. Edits preserve identity, and saving a new profile ID
always generates a new identity even if a copied object contains an existing UUID.
The endpoint is the HTTP TCP control listener. `server_identity` defaults to
`cpxy-rtp`; its 1–255 byte UTF-8 limit is enforced in Kotlin and Rust.

The owned UniFFI preparation validates configuration/key/UUID before socket use,
exposes cancellation independently of the blocking worker lane, returns an address
only after authenticated UDP readiness, and transfers the same path into the
worker exactly once. Every valid transferred TUN descriptor is closed on failure,
including repeated start. `Connected` still requires ACTIVATED. Protection refusal
aborts TCP/UDP use. Cleanup and allocation release run off the Android UI thread.
Endpoint and DoT/DoH hostname lookups use the same cancellation boundary. DNS
addresses are pinned in the prepared settings while retaining the original TLS
server name, so TUN handoff cannot begin a second system lookup.
System getaddrinfo cannot be interrupted portably: at most two detached resolver
workers exist, the caller waits at most five seconds, checks cancellation every
20 ms, and late results contain addresses only. Candidate races also check caller
cancellation every 20 ms. Cancellation shuts down in-flight HTTP and prevents new
control sockets; an already-created lease may therefore expire on the server
instead of completing best-effort release. All HTTP attempts have finite deadlines.

The service retains its own TUN descriptor and transfers a duplicate to each
worker. Failed workers are stopped before a replacement reads that TUN. Explicit
Disconnect/revoke/destroy and permanent errors close both references. A changed
assigned address establishes a replacement before closing the service reference
to the old interface. Generation guards and main-thread final TUN installation
prevent cancelled preparation results from installing a late interface. The
platform replacement's routing-gap behavior remains a device acceptance gate.

Backup is disabled. Explicit cloud/transfer exclusions also cover profile files
and encrypted key preferences, so ordinary backup/device transfer cannot clone
UUIDs. Manual copying of private app data remains outside this guarantee. Clearing
app data/uninstalling creates fresh identities; the server retains old permanent
address mappings. Keystore loss/corrupt ciphertext asks for the password again
and preserves the profile UUID. Runtime allocation secrets/counters are never
persisted.

## Results so far

- Rust unit/loopback suite passed (the existing production five-minute test is
  ignored by default). Added native cancellation, invalid-input, false protection,
  descriptor ownership, and caller-cancel race coverage.
- Kotlin shared JVM tests: 19 passed, covering legacy JSON, ID/selection/DNS,
  durable UUID transactions under concurrent access, edits/copies and corruption.
- A real loopback HTTP/UDP regression passed: after restarting the server with a
  different address pool, the existing worker returns `AddrNotAvailable`, allowing
  the platform to prepare a replacement TUN instead of retrying a stale address.
- Regenerated UniFFI bindings and Android debug Kotlin compiled successfully.
- `rtp-mobile/tests/linux-lab.sh` passed in isolated user/network/mount namespaces
  for outer IPv4, outer IPv6, blocked IPv4 UDP and blocked IPv6 UDP. This covers
  tunneled TCP/UDP, direct regional traffic, split DNS, AAAA suppression, refusal,
  address-change path repair, and endpoint-freeze fail-closed checks on Linux.
- Full four-ABI debug/release builds and Android instrumentation are in progress.

## Local Android fixture

`mobile-rtp/tests/local-endpoint.py` starts a real endpoint, HTTP/UDP echoes and
split-DNS responders in a private network/mount namespace. Only loopback relays
are installed on the host. It uses deliberately fixed disposable test credentials;
never point these tests at a public endpoint. UDP relays preserve application
packets but normalize the endpoint-visible source tuple, so this fixture cannot
prove real Wi-Fi/cellular roaming. No host route/firewall/NAT changes are made.
State/logs live only under the selected temporary directory.

```bash
cargo build --locked -p rtp-service -p rtp-mobile --bins
sudo python3 mobile-rtp/tests/local-endpoint.py /tmp/rtp-android-fixture
# In another terminal, boot a disposable emulator, then from mobile-rtp/:
./gradlew :androidApp:connectedDebugAndroidTest
```

Instrumentation uses emulator host alias `10.0.2.2`, HTTP control port 33000,
UDP allocation ports 33001–33016 and direct fixture DNS on 33053. It grants VPN
consent with shell appops on the disposable emulator. Production rotation is opt-in:

```bash
./gradlew :androidApp:connectedDebugAndroidTest \
  -Pandroid.testInstrumentationRunnerArguments.class=dev.fanchao.cpxy.rtp.RtpLifecycleTest#productionRotation \
  -Pandroid.testInstrumentationRunnerArguments.rotation=true
```

Hardware-only Wi-Fi/cellular handover, screen-off/battery behavior, and protection
after process death without Android always-on lockdown are not claimed here.
Initial preparation before a VPN exists does not block all system traffic.


## Checkpoint before latest-main merge

At the user's requested merge checkpoint, 77 Rust tests and 19 Kotlin JVM tests
passed. The API 36 / Android 16 x86_64 emulator suite passed 10 tests, with the
settings-driven revocation check and production rotation reserved for separate
runs. The separate production rotation passed with continuous TCP traffic; the
endpoint prepared UDP port 33007 while 33005 remained active, then retired 33005
after handover. Both final APK variants built successfully with all four ABIs.
The final Linux matrix also passed for 4, 6, block4 and block6. Packet-capture
analysis, the settings-driven revocation check and final delivery remain in progress.
These results precede merging the latest main and will be rechecked where affected.
