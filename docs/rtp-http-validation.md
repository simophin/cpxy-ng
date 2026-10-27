# HTTP-controlled RTP v2 validation

Date: 2026-10-28. Scope: Linux core and opt-in OpenWrt integration on
`feat/http-controlled-rtp`. No public or cross-host deployment was performed.
Historical v1 reports remain historical; results below exercise the v2 code.

## Implementation and contract

The service uses encrypted JSON-RPC over always-plain HTTP, random per-request
paths, short-lived generation-scoped challenges, bounded replay/deduplication
caches, stable UUID/address persistence and dedicated admitted UDP allocations.
UDP uses XChaCha20-Poly1305 with allocation/direction-separated HKDF material,
monotonic counters and a 1024-packet receive bitmap. There is no legacy wire
negotiation, raw stream cipher, base UDP fallback or server rotation epoch.

One TUN reader dispatches to bounded per-client FIFOs. HTTP/firewall work runs
in an independent serialized owner without holding the packet table lock across
privileged I/O. Generation-scoped admission remains in `port-admission`.

The [v2 contract](rtp-http-protocol.md) specifies exact encodings and defaults.
Two implementation refinements were necessary for restart/handover correctness:

- A create returns `activation_floor`, derived from the current route watermark.
  The client owner chooses at least that number. A restarted process can replace
  its stale route using a fresh allocation without persisting or resetting any
  packet counter. Superseded allocations expire after their drain deadline.
- The seven-second activation retry budget starts when ACTIVATE begins, after
  candidate preparation. A prepared candidate can wait for the five-minute
  deadline without consuming its activation budget.

Address allocation before TUN creation retries transient connection failures
for a bounded startup interval, including simultaneous server/client startup.
Static claims are persisted before dynamic allocation can collide. UUIDs are
consistency identifiers for trusted PSK holders, not independent credentials.

## Commands and results

| Check | Result |
| --- | --- |
| `cargo test --locked --offline -p rtp-l3 -p rtp-service -p port-admission -p rtp-mobile` | 64 tests passed: shared codec/control/HTTP 19, service 26, admission 7, packet/DNS helpers 12. The long rotation test is ignored in this ordinary invocation and was run separately. |
| `cargo test -p rtp-service five_minute_rotation --offline -- --ignored --nocapture` | Passed in 311.09 seconds with continuous bidirectional packet traffic; activation near 300 seconds and one remaining lease after drain. |
| `cargo check --workspace --locked --offline` | Passed, including shared mobile API consumers. One preexisting `AsyncFd::new` deprecation warning in `mobile-engine`. |
| `cargo build -p rtp-service -p rtp-mobile --bins --offline` | Passed. |
| `cargo fmt -p rtp-l3 -p rtp-service -- --check`, `git diff --check`, shell syntax checks | Passed. |
| Independent Python/HMAC/HKDF + system libsodium oracle | Both UDP directions and HTTP request/response vectors match the Rust implementation. Public fixtures and generator are checked in. |
| `bash rtp-mobile/tests/linux-lab.sh` | `RTP_MOBILE_LAB_OK`; real v2 TUN TCP/UDP, direct routing, DNS split, refusals, path change and endpoint-loss behavior. |
| `cargo clippy -p rtp-l3 -p rtp-service --all-targets --locked --offline -- -D warnings` | Not run: Clippy is absent from the pinned local toolchain. No lint pass is claimed. |

Real Linux fixtures were invoked inside disposable local mount/network
namespaces. The host lacked `/dev/net/tun`; rootless device creation was denied.
Noninteractive sudo with a private `/dev` supplied a TUN entirely inside those
namespaces. The checked-in wrapper refuses the host network/mount namespaces
and stages only the executable in a private temporary directory:

```sh
sudo unshare -nm bash rtp-service/tests/private-linux-lab.sh FIXTURE [ARGS...]
```

| Fixture | Result |
| --- | --- |
| `rtp-service/tests/linux-lifecycle.sh` | Passed real TUN traffic, graceful stop/restart and released sockets/journals. |
| Same fixture with `RTP_TEST_REPAIR=1` | Passed dynamic addressing, persisted UUID/address recovery, deliberate UDP-port blackhole, failed same-allocation path repair, HTTP recreation, acknowledged recovery and restart. |
| `rtp-service/tests/linux-applications.sh` | `HTTP_APPLICATIONS_OK`: actual TCP/UDP, process death/restart, retained unreachable guards, outage behavior, unrelated route/rule preservation, explicit cleanup and startup collision checks. |
| `rtp-service/tests/linux-namespace-broker.sh __inner` | Passed real protected TCP HTTP and UDP descriptor brokerage into the nested unprivileged client namespace. |
| `rtp-service/tests/linux-dual-stack.sh __inner` | `INNER_DUAL_STACK_OK`: concurrent IPv4/IPv6 outer paths, IPv4 inner traffic, separate endpoints and namespace clients. |
| `rtp-service/tests/linux-internet.sh` | `INTERNET_EXIT_OK`: explicit gateway routing/NAT, TCP/UDP, router-local bypass and crash guards. |
| `rtp-service/tests/linux-firewalld.sh` | `HTTP_FIREWALL_NAMESPACE_OK`: actual firewalld on a private D-Bus, scoped rules/forwarding/masquerade, conflicting administrator rules, hung backend, reload/restart, SIGKILL and repeated independent cleanup. |
| Same firewalld fixture with `RTP_TEST_REPAIR=1` | Passed UDP-blackhole recovery and the firewall lifecycle suite together. |
| Same firewalld fixture with `RTP_TEST_OS_PORTS=1` | Passed with no UDP range: actual OS-assigned ports carried TUN/application traffic, survived reload/restart, and were removed by crash/independent cleanup. |

The firewalld fixture explicitly provisions its administrator-owned TCP control
admission separately from service-owned UDP rich rules. Unrelated runtime and
permanent rules survive the checks. These are real firewall mutations confined
to the fixture network namespace, rather than mock-adapter evidence.

## Focused protocol coverage

Tests cover every-byte tampering, wrong keys/directions/allocation context,
truncation, counter exhaustion, RTP rollover, replay-window boundaries and
reordered packets. IPv4 data reconstruction is byte exact in both directions,
including minimum/maximum sizes, options and fragment/checksum bytes. Inner IPv6
is rejected; packet boundaries expose an IPv6-drop counter.

Control tests cover challenge replay/consumption/expiry, capacity, generation
changes, random path shape, unrelated short paths, uniform 404s, strict framing,
address persistence/reservations/conflicts/exhaustion, operation conflicts and
retry horizon. Real concurrent duplicate creates return the same allocation and
secret while preparing only one lease. Release is idempotent; a released create
returns terminal status. Injected failed commit/retirement keeps the socket
reserved; stale release cannot affect numeric-port reuse.

Real UDP tests discard an ACTIVATED reply, retry the same activation with a fresh
packet counter, and recover the committed route. They also verify pending FIFO
flush, roaming validation without premature redirect, explicit path activation,
and rejection of delayed attempts to restore the old tuple. Deterministic health
tests cover incoming-only NAT maintenance, active-upload return-path probes,
matching PONGs and nonextendable 1/2/4-second retry deadlines. Queue tests cover
expiry, pressure and FIFO order. These checks are not a zero-loss guarantee.

## Optional outbound interface follow-up

The client now uses normal OS routing when `--outbound-interface` is omitted.
An explicit override still applies SO_BINDTODEVICE before connecting HTTP or UDP
sockets. This flag does not add routes. Namespace clients always obtain sockets
from the host broker, using host routing by default, so their child tunnel routes
do not capture control or repair traffic.

The baseline full-suite results above predate this follow-up. Focused validation
of the optional-interface change:

| Check | Result |
| --- | --- |
| `cargo test -p rtp-service --lib --locked --offline` | 28 passed, one long rotation test ignored. Covers missing/invalid/explicit interface settings and default/explicit TCP/UDP socket binding. |
| `cargo test -p rtp-service broker::tests --lib --locked --offline` | Six passed, including socket-option checks confirming unbound defaults and retained explicit UDP/TCP interface binding. |
| `cargo build -p rtp-service --locked --offline` | Passed. |
| `RTP_TEST_REPAIR=1` lifecycle fixture via the private Linux wrapper | `HTTP_NAMESPACE_OK`: dynamic addressing, TUN traffic, UDP blackhole recovery over HTTP, and restart passed with no outbound-interface argument/environment value. |
| Namespace broker fixture via the private Linux wrapper | Passed with no outbound-interface argument: HTTP and UDP sockets remain in host networking while the child has only its tunnel route. |
| Formatting, whitespace and fixture shell syntax | Passed. |

The existing selected-network route and fail-closed guard logic is unchanged.

## Automatic UDP port selection

When `--udp-ports` is omitted, the server binds UDP port zero and lets the OS
select an available port. The TCP control port does not determine a default
UDP range. Explicit nonzero LOW-HIGH ranges remain supported.

The socket remains bound while its actual address is journaled and admitted.
Firewall rules, commit/retirement calls and HTTP results always use that actual
nonzero port. Automatic admission delegates individual port leases across the
UDP port space; it never installs a range-wide rule. Failed or uncertain cleanup
continues holding the socket so the kernel cannot reuse it.

Additional tests cover two simultaneous clients with OS-assigned UDP ports,
actual-port journaling/commit/retirement, and explicit-range validation. The real
isolated firewalld suite also passes in automatic mode and is included in CI.

## Defaults and wire overhead

| Policy | Value |
| --- | --- |
| UDP port selection | OS-assigned by default via bind(:0); optional explicit LOW-HIGH range |
| Inner IPv4 MTU | 1200 bytes |
| Startup / inactivity | 30 / 120 seconds |
| Activation-to-activation rotation / preparation lead / drain | 300 / 10 / 10 seconds |
| Idle keepalive | 15 seconds, +/-20% jitter; configurable 5..45 seconds |
| Inbound health idle / probe waits | 15 seconds; 1, 2, 4 seconds |
| Challenge lifetime / operation retry horizon | 30 / 600 seconds |
| Challenge / operation / allocation limits | 4096 / 8192 / 1024; two live allocations per UUID |
| HTTP headers / body / request target | 8192 / 16384 / 2048 bytes |
| HTTP concurrency / connection deadline | 64 / 5 seconds |
| Pending payload age / per-client / global | 1 second; 256 packets or 256 KiB; 16 MiB total |
| Downstream batch | Up to 32 packets per allocation per turn |

The UDP frame has a 12-byte RTP header, 8-byte packet number and 16-byte tag.
Omitting one four-byte inner address produces net overhead of 32 UDP-payload
bytes, or **60 outer IPv4 bytes** above the original inner packet. A 1200-byte
inner packet yields a 1232-byte UDP payload, **1260 outer IPv4 bytes**, or
**1280 outer IPv6 bytes**. Control messages retain their complete bodies.

## Cleanup and limits

All fixture service/backend processes were terminated and reaped. The firewall
fixture now records and reaps its private D-Bus daemon too; task-private daemons
left by earlier fixture revisions were removed. Failed debug staging was
removed. Final inspection found zero task-private service/D-Bus processes,
zero executable staging directories, and no fixture interfaces in host
networking. The private `/dev`, routes, nftables rules and namespace resources
were released with their namespaces. No private keys were generated in the
checkout; Linux fixture passwords existed only in process environments. QEMU generates a
separate temporary password; its private serial log contains setup commands and
is discarded with the fixture artifacts.

Android app migration remains explicitly outside this PR's scope; its binding
rejects startup. The shared Rust crate historically named `rtp-mobile` now
supplies the Linux/OpenWrt packet/DNS engine, with a real v2 Linux lab. No mobile delivery, throughput claim, public deployment,
inner IPv6, FEC, reliable data delivery or forward secrecy is included.

Other timers and cache/queue ceilings currently use the pinned v2 defaults;
only idle keepalive is configurable. Exposing the full timing policy with
cross-field validation remains follow-up work from the plan. The tests do not
constitute exhaustive fuzzing, every cancellation boundary, or a comprehensive
netem loss/reorder matrix. Independent UDP reordering/replay and real path-loss
checks are reported only at their demonstrated scope.


## Linux/OpenWrt extension (2026-10-28)

The user requested replacing the undeployed RTP VPN while retaining the original
`cpxy-router`, then clarified Linux/OpenWrt only. Android app files are unchanged
from before this extension. HTTP client framing, identity persistence and the
UDP client owner now live in `rtp-l3::v2`; Linux service and router packet/DNS
engines use that same owner. No second RTP protocol or rotation implementation
was introduced.

The opt-in `cpxy-rtp` package supplies a separate `cpxy_rtp` UCI file, procd
service, nonpersistent `rtpr0`, regional route generations, selected-LAN terminal
guards and an independent nft forwarding/DNS guard. A private validated UCI
snapshot isolates crash recovery from rejected edits. Guards are restored before
netifd at boot, while the packet worker starts after network initialization.
The former combined early startup exposed a boot-ordering failure during QEMU
testing and was split accordingly. Explicit cleanup releases the policy;
stopping/crashing does not release it.

Additional completed checks:

- Focused Rust suite: 64 passed, one production-length rotation test remains
  separately exercised as recorded above. The new callback test observes both
  TCP and UDP descriptors before connection and checks control-socket cancellation.
- Workspace compilation and formatting passed. The existing unrelated
  `mobile-engine` AsyncFd deprecation remains.
- Real Linux packet/DNS namespace lab passed against HTTP-controlled v2, including
  TCP/UDP, direct routing, DNS selection, refusals, path repair and fail-closed
  endpoint loss. Android UI/native delivery is excluded.
- Static x86-64 and ARM64 musl builds passed for both executables. The ARM64
  toolchain emits a verbose linker-wrapper warning but exits successfully.
- IPK installs in OpenWrt QEMU; APK v3 builds and metadata checks passed. Actual
  APK installation on an APK-based OpenWrt release and physical router performance
  remain untested.
- ShellCheck passes for new package/service scripts. Actionlint structure checks
  pass; its full ShellCheck mode reports existing unrelated workflow advisories.

The reproducible [QEMU fixture](../packaging/openwrt/test/qemu-rtp.py) uses the
[official OpenWrt 24.10.0 image](https://downloads.openwrt.org/releases/24.10.0/targets/x86/64/),
SHA-256 `a873d9f2e6e667d03dd06f75ed1f1b580beb7ce4337d5a44b60911a266965fb5`,
matching `6.6.73-r1` TUN module and verified feed dependencies. It runs with
software emulation, three isolated Ethernet segments and a separate local RTP
endpoint, using a disposable disk snapshot. The fixture checks preservation of
an enabled original-router configuration on the unselected LAN; it does not
claim simultaneous packet tests of both router binaries.

Final QEMU run passed all assertions:

| Scenario | Observed result |
| --- | --- |
| Selected guest LAN TCP and UDP | Regional destinations saw WAN source `192.0.2.2`; nonregional destinations saw assigned RTP source `10.88.0.2` |
| Unselected LAN | Continued ordinary WAN forwarding |
| UDP and TCP DNS | Regional/direct and alternative/through-RTP answers; AAAA suppressed |
| IPv6 | A working external IPv6 baseline was demonstrated before refusal; local router IPv6 stayed reachable; IPv6 DNS to the router was promptly refused |
| Service and firewall reload | All four TCP/UDP paths recovered |
| Suspended packet worker | Regional TCP/UDP remained direct; nonregional TCP did not bypass RTP |
| WAN gateway update through netifd | Direct routes reconciled and RTP forwarding recovered |
| Retained-router overlap | Rejected before replacing the running worker |
| SIGKILL | procd restarted the worker; UUID and assigned IPv4 remained stable |
| VM reboot | Early guards and late worker restored forwarding and the same assigned IPv4 |
| Endpoint loss and explicit stop | Nonregional traffic stayed blocked; regional direct routing continued |
| Explicit cleanup and IPK removal | Ordinary WAN routing returned; original configuration checksum remained unchanged |
| Transition monitoring | Continuous selected-LAN TCP probes plus WAN TCP/UDP capture found no unintended direct nonregional bypass |

The fixtures were corrected during development for serial-console line limits,
network-service readiness, the endpoint's required IPv4 forwarding, exact UDP
reply source addresses and an unavailable host DNS client. The final run uses the
repository's standard-library DNS client and exits successfully. Emulation found
and verified the separate early/late boot lifecycle and scoped IPv6 DNS guard.
No OpenWrt hardware or throughput acceptance is claimed. IPK/APK builds and QEMU
checks are wired into CI; remote CI results are separate from these local passes.
