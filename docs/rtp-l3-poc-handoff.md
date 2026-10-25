# RTP-framed L3 tunnel POC handoff

## Scope and source of truth

Branch: `poc/rtp-l3-tunnel`. The standalone Rust crate is `experiments/rtp-l3`;
production proxy code and desktop/mobile integration are outside this POC.

There is one [protocol specification](rtp-l3-protocol.md), covering data,
authenticated control, multi-client registration, rotation, keepalive and
fallback. The [crate README](../experiments/rtp-l3/README.md) documents commands
and counters. Earlier experiment configurations are historical measurements,
not supported runtime modes. Both peers must use matching current builds.

Current consolidation removes experimental version labels from key derivation
and runtime reporting, and makes the control header's second byte reserved
zero. Packet sizes and lifecycle rules are unchanged. Both peers must be
rebuilt together; the historical cross-host measurements do not validate these
new key labels/header bytes. No production compatibility or legacy decoder is
needed. Standard RTP version 2 and IPv4 are unrelated to those former labels.

## Current state

The runtime and namespace matrix are implemented. Public-path measurements
completed, including a failed idle-retention criterion. The ten-minute rotated
upload delivered 4.968 Mbit/s with 0.626% loss. Ten subsequent idle minutes
produced 49 base fallbacks. A separate five-second keepalive diagnostic also
fell back (three times; stopped after 147 seconds total). No validated
keepalive setting or exact return-path cause is claimed. Post-idle traffic
recovered; reverse UDP loss was 14.725%.

The scheduled-rotation versus fixed-port comparison completed after one UDP
setup failure and one bounded remaining-case retry. Fixed mode disables
scheduled rotation but still recorded one fallback. Single samples, a restart,
return-path loss and local server receive-buffer drops prevent a causal
performance claim. These are experimental results, not a finished app or a
production-readiness claim.

Results: [cross-host report](rtp-l3-cross-host-results.md), with
[endpoint-free evidence](../experiments/rtp-l3/results/2026-10-25-z4-z6-summary.json).
Historical records: [AEAD baseline](rtp-l3-aead-results.md),
[24-byte random nonce experiment](rtp-l3-random-nonce-results.md), and
[header-derived nonce data experiment](rtp-l3-data-format-results.md).
Their JSON identifiers/filenames are frozen measurement provenance and are not
current protocol names. Removed implementation history remains in Git.

## Acceptance and next work

- [x] Z0–Z2: control codec, key separation, session state machines, Linux runtime,
  CLI/counters, unit tests and inherited-socket integration.
- [x] Z3: two clients behind disposable masquerade, sustained transfers across
  rotations, lost announcements/secret recovery, conntrack rebinding, blocked
  rotation-port fallback and live server restart.
- [ ] Z4: public-path idle retention. Measurements ran; acceptance **failed**.
- [x] Z5: bounded fixed-port/rotation comparison, including receiver loss,
  runtime-correlated switch windows, packet shape and base-port observations.
- [x] Z6: verified firewall/network/process cleanup and sanitized report.

The first diagnosis item completed with opt-in correlated traces and a bounded
public-path run: [control completion report](rtp-l3-control-trace-results.md),
[endpoint-free evidence](../experiments/rtp-l3/results/2026-10-25-control-trace-summary.json).
All nine server-captured replies in three exhausted PING groups were absent
from client runtime reception; every received solicited STATE was accepted.
Two ignored unsolicited bootstrap announcements did not explain fallback.
Three-minute application-idle diagnosis reproduced three fallbacks and failed
post-idle MTU ping (0/5). It is not a ten-minute Z4 acceptance run. No exact NAT
cause or fix is claimed.

Controlled delayed and repeated immediate probes now show destination-port
dependence on the current path: four external mappings from one local socket,
all same-source immediate controls delivered, no cross-source returns, and
silent same-source delivery at 15 seconds but none at 30–180 seconds.
See the [independent delayed-probe report](rtp-l3-idle-matrix-results.md) and
[sanitized evidence](../experiments/rtp-l3/results/2026-10-25-idle-matrix-summary.json).
The historical one-mapping immediate baseline is unchanged. Neither sample
identifies the responsible device or establishes the tunnel-specific cause.

The explicit next task is **match tunnel PING/STATE traces to the observed
mapping, source-port and idle behavior**, before accepting Z4:

- [x] Trace fresh PING sequences, server reply generation/transmission, client
  receipt, decoder result and state-machine acceptance with synchronized,
  endpoint-free event correlation. Distinguish packets missing on the return
  path from replies received but ignored or superseded by a newer sequence.
- [x] Repeated immediate destination-port baseline: one socket, randomized
  four-port requests and cross-source replies; 192/192 delivered, one mapping.
- [x] Repeat delayed same/cross-source probes on independently silent sockets
  with separate start/deadline immediate controls and server-interface evidence.
  The raw-receive companion recorded zero rejected datagrams. Current immediate
  contact-history comparison found four stable destination-dependent mappings;
  48/48 same-source replies and 0/144 cross-source replies arrived. This differs
  from the prior 192/192 one-mapping baseline; preserve both samples.
- [ ] Compare matched tunnel traces and socket behavior with these plain-UDP
  observations, and locate return loss with client-interface evidence where
  available. The current unprivileged client lacks capture tools/capabilities;
  server capture alone cannot locate loss. No exact NAT timeout or device is
  identified, and the earlier five-second keepalive failure remains unexplained.
- [ ] Establish the cause before changing socket/port strategy or keepalive
  timing. One local client socket remains the design; rotating local ports is
  only a hypothesis for a later experiment. Shortening keepalive to five
  seconds did not resolve the observed failure.
- [ ] After a justified fix, repeat the ten-minute continuous paced UDP/ping
  rotation run and ten-minute application-idle run, followed by MTU ping and
  reverse UDP. Confirm retained connectivity and investigate unexplained
  fallback; choose keepalive from evidence rather than assuming success.
- [ ] Update acceptance, preserve failed attempts and matched measurement
  settings, publish sanitized evidence, and verify cleanup again.

The first trace/completion diagnosis is complete; the loss location and cause
remain unresolved. Neither instrumentation nor the bounded diagnosis completes Z4.
Keep the control instrumentation available for the next controlled experiment.

## Validation record

Before naming consolidation: 18 portable library tests plus one Linux
inherited-socket test passed, with offline Linux release build and clippy
warnings denied. The two-client namespace matrix's evidence is
[the Z3 aggregate](../experiments/rtp-l3/results/2026-10-25-z3-summary.json).
Each client transferred at least 100 MiB each way across multiple ports;
receiver interval stall bounds were below five seconds. Recovery faults passed.

Cross-host receiver/capture evidence and limitations are in the report, including
49 idle fallbacks, an initial UDP setup failure, the fixed-client fallback,
server receive-buffer drops and substantial download loss. Shape/gap parser
and bounded-continuation fixtures passed. Historical success is not a new test
of consolidated key/header conventions. Consolidation validation passed: 18 portable tests on macOS; 19 tests,
warning-denied clippy, offline release build, single-client namespace regression
and the full two-client/recovery matrix on Linux. Each client transferred at
least 100 MiB each way, crossing five new ports on upload and six on download;
the largest conservative transfer stall bound was 1.004 s. All recovery checks
passed. The first matrix setup stopped before testing because iptables was
outside the unprivileged PATH; adding admin-tool directories resolved it.
[Consolidated-protocol namespace evidence](../experiments/rtp-l3/results/2026-10-25-unified-namespace-summary.json)
is separate from the untouched historical measurement JSON. This validates
matching updated peers locally, not public-path idle retention. Z4 diagnosis
remains open. Validation staging/builds, namespaces and the private key were
removed after checking task-process absence; the public server was untouched.

## Authorized environment and hygiene

Use the authorized unprivileged Linux client and explicitly scoped root server
from the active private session context. Never put actual hosts, Linux users,
endpoints, private paths, SSH configuration, keys or raw captures in Git.
Generate keys outside the checkout, restrict files to mode 0600, never print
keys, and transfer privately. Stage only task files in private temporary
locations. Remote fish shells require wrapping commands in `sh -lc`; add
`$HOME/.cargo/bin` to the remote PATH; namespace firewall tests also need
`/usr/sbin:/sbin` for tools such as iptables.

Prefer disposable user/network namespaces for client and local integration;
no client sudo, host route or host sysctl changes. Inspect server networking
before selecting temporary ports, interface names and address ranges. Use
source-/32-restricted firewalld **runtime rules only**, with explicit cleanup,
and preserve unrelated services. Snapshot/compare runtime zones/policies,
links and any settings touched. Remove task processes, namespaces, interfaces,
staging/build files and every key copy, including on failures. Raw identifying
artifacts stay private; publish sanitized aggregates only.

For GitHub operations, safely inspect the repository `.envrc`, explicitly apply
its selected personal GitHub environment, verify the intended login before a
write, and use that same environment for Git credentials. Do not assume direnv
has loaded. Follow the active AGENTS instructions for identity and access.

## Cleanup and commit discipline

The completed measurement deployment was removed. Runtime zones/policies and
links exactly matched their pre-task snapshots; IPv4 forwarding was unchanged,
permanent firewall configuration untouched, and task processes absent on both
hosts. Remote and local staging/builds, captures and all key copies were
removed. Consolidation validation resources were also removed after successful checks.
No public-server deployment or cleanup obligations remain.

Commit each coherent checkpoint with handoff updates. Before interruption,
record exact next work, current validation and any cleanup obligations. Never
rewrite historical measurements to imply the current implementation passed.

## Control trace checkpoint

Added opt-in `POC_TRACE_CONTROL`, explicit accepted/ignored STATE outcomes,
wall/monotonic microseconds and encrypted-datagram fingerprints. The analyzer
joins fresh PING sequences to STATE sends, optional server-capture fingerprints,
client reception, decoder and state outcomes, publishing numbered ports/sessions.
Protocol and retry/keepalive policy are unchanged. Validation: 19 portable tests,
20 Linux tests, warning-denied Linux clippy, offline release build, four analyzer
fixtures and final traced namespace regression (100 MiB each way, idle rotation,
restarts and UDP faults). The first namespace attempt exposed the harness's
ready-first-log assumption; two runs passed after allowing startup trace events.
The public sample preceded a final allocation/trace-enable optimization only;
final code passed the Linux checks and namespace regression. Server runtime
firewall/network snapshots were restored exactly, task-process/interface absence
verified, and both hosts' staging/builds/keys plus local private captures and keys
removed. No outstanding deployment or cleanup obligations remain.

## Destination-port diagnosis checkpoint

Added `scripts/port-matrix.py`: bounded repeated immediate plain UDP probes,
randomized destination order, one client socket, all-source replies and sanitized
numbered mapping/delivery output. Private server JSONL records observed tuples
and successful send calls. Loopback integration and CLI-bound fixtures passed
with network access enabled (the sandbox forbids UDP sockets). Public-path
measurement completed: 48/48 same-source and 144/144 cross-source replies, with
one server-observed mapping. This immediate plain UDP baseline does not reproduce
the tunnel failure. [Report](rtp-l3-port-matrix-results.md) and
[sanitized evidence](../experiments/rtp-l3/results/2026-10-25-port-matrix-summary.json)
preserve its settings and limits. Z4 and the cause-isolation TODO remain open.
Next: repeated delayed probes on independently silent sockets with matched
immediate controls and client-interface evidence, then matched tunnel traces.
Server runtime firewall/link snapshots matched after cleanup; a subsequent check
verified all test ports free and remote staging absent. Private logs/staging and
local probe artifacts were removed. No outstanding cleanup obligations remain.

## Independent delayed-probe implementation checkpoint

Added `scripts/idle-matrix.py` with randomized repeated destination/delay trials,
one reply per silent socket and separate start/deadline immediate-control
sockets. All sockets remain open through the run to avoid local port reuse.
Fixed 192-byte plain UDP payloads match each other, not the tunnel wire format.
Private mode-0600 logs record correlation and endpoint evidence;
`scripts/idle-evidence.py` publishes endpoint-free server receipt/send, client
receipt and optional interface-capture fingerprint joins. No tunnel protocol,
socket strategy, rotation or keepalive setting changed.

Four delayed-probe fixtures and both existing immediate-matrix fixtures passed
with UDP socket access. They cover actual delayed same/cross-source delivery,
socket isolation, missing replies, bounded CLI/private-log handling and passive
capture correlation on four link types. The initial sandbox attempt could not
create UDP sockets; the same tests passed with network access enabled.
Public-path delayed measurements are pending at this checkpoint. Z4 remains
failed/open; local fixtures cannot establish public-path retention or its cause.

The client additionally records every socket receive before source/token/body
validation. The analyzer distinguishes accepted replies, rejected datagrams and
no socket delivery, retaining null counts for older logs. Five fixtures now
pass, including injected wrong-token replies followed by valid replies. This
instrumentation does not alter probe framing, timing or tunnel behavior.

## Public delayed/mapping diagnosis checkpoint

The bounded delayed matrix, overlapping raw-receive companion and immediate
contact-history comparison completed. Details and excluded setup/lifetime
attempts are preserved in the [report](rtp-l3-idle-matrix-results.md) and
[endpoint-free artifact](../experiments/rtp-l3/results/2026-10-25-idle-matrix-summary.json).
All 416 immediate controls arrived. Same-source silent returns arrived through
15 seconds and failed at 30 seconds and above; every cross-source return was
absent. A subsequent single-socket matrix observed four destination-dependent
external ports, 48/48 same-source returns and 0/144 cross-source returns.
Every valid request/reply appeared in server-interface captures, with zero
kernel capture drops. The companion recorded no rejected client datagrams.
These are current-path plain-UDP observations, not a tunnel fix or Z4 pass.

Five delayed fixtures and both immediate fixtures passed. The server used the
committed implementation; the receive-instrumented companion used the subsequent
instrumentation checkpoint, with no wire/scheduling change. All server runtime
and permanent firewall snapshots, links and forwarding matched after cleanup;
task processes exited, test ports were free and staging was removed. Local
private artifacts were removed after sanitization. No cleanup obligations remain.
Next: matched tunnel traces, then justified remediation and Z4 revalidation.


## Z4 repair implementation checkpoint

The baseline actual tunnel reproduced a complete return failure despite
server-accepted authenticated PINGs and exact reverse-tuple STATE transmission.
A same-destination, same-payload cohort observed persistent socket-specific return
failures across wildcard, explicit-address, disconnected and connected bindings:
11/32 sockets received 0/3 echoes, 20 received 3/3 and one received 2/3. The server
captured 95/96 requests and 95 echoes across all 32 tuples. This does not locate
the failing device, but identifies why retries on a fixed failed tuple cannot
recover. Earlier private shape/language comparisons varied with fresh sockets;
neither Rust nor RTP framing was a consistent discriminator.

The repair keeps the established route while authenticating a candidate with
PROBE/STATE. PROBE cannot create/move a session; only registration PING commits
routing. A launcher-owned host socket broker replaces failed candidate/base
mappings without restarting TUN/apps. Candidate retries use three full one-second
monotonic deadlines; keepalive remains 15 seconds ±20%. TUN/application startup
waits for confirmed registration. Both peers need the updated build.

Initial validation: 21 library tests and the Linux inherited-socket test passed,
warning-denied clippy passed, and the existing 100 MiB/restart/UDP-fault regression
passed. A new namespace fault regression dropped four candidate replies, renewed
its socket, and delivered all 650 pings with every paced UDP receiver interval
nonempty and zero fallbacks. A preliminary public 180-second idle diagnostic
passed with six switches, zero fallbacks and 5/5 post-idle MTU pings. That sample
preceded final monotonic candidate timing and broker/poll cleanup refinements;
final acceptance remains pending. Every completed deployment restored runtime
and permanent firewall snapshots, links and forwarding and removed processes,
TUNs, remote staging and key copies. Private local evidence/builds/key remain
outside the checkout while the repair is being validated. Next: final-code
namespace checks and the complete ten-minute active/ten-minute idle Z4 run;
keep Z4 open until acceptance and final cleanup are verified.
