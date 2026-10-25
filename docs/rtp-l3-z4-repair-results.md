# Z4 public-path retention repair

Z4 **passed** on matching updated peers: ten minutes active, ten minutes
application idle, no base fallbacks, successful MTU ping and reverse UDP.
The previous failed measurements remain preserved.

Evidence: [sanitized acceptance and diagnosis](../experiments/rtp-l3/results/2026-10-25-z4-repair-summary.json).
Both peers used the same release binary from implementation commit `fb754b7`;
the measurement harness includes the text-report parsing fix. Public UDP used
the server's globally routable IPv4 on its external Ethernet interface. SSH
management used a separate overlay address; no overlay endpoint was used for
measured tunnel traffic. Host identities, addresses, ports and private artifacts
are excluded from the published evidence.

## Failure mechanism and repair

The old runtime reproduced complete missing return delivery despite authenticated
PING acceptance and exact reverse-tuple STATE transmission. Direct host and
inherited sockets could both fail. A same-payload, same-destination cohort used
32 sockets across wildcard, explicit-address, disconnected and connected modes:
11 received 0/3 echoes, 20 received 3/3, and one received 2/3. The server captured
95/96 requests and 95 replies across all 32 observed tuples. Fresh-socket shape
comparisons did not consistently distinguish JSON, RTP echo, authenticated
control or programming language. The responsible network device remains unknown.

The protocol previously switched before testing a new return path and retried
persistently failed tuples. The repair adds authenticated PROBE/STATE validation
while traffic remains on the established socket. PROBE requires an existing
session and cannot move its data route. Only a subsequent registration PING
commits the validated tuple. Three unanswered one-second candidate challenges
request a fresh host-network socket from the launcher's private descriptor broker.
At most base, active, previous and candidate sockets are retained by the runtime.

A partially completed active acceptance attempt also exposed a later failure
of an already validated flow. Active uploads now request liveness after two
seconds without input. A timed-out PING starts a same-port repair on a separate
candidate descriptor, preserving the old descriptor until proof completes.
Registration PING is required even when the server port/epoch stays the same.
An eight-second repair budget preserves eventual base recovery after server
restart. Reordered advertisements cannot replace newer pending candidates.
Applications start only after validated registration; TUN/apps survive repairs.

## Final acceptance

One client session ran ten minutes of paced 5 Mbit/s UDP plus ping, followed by
ten minutes with no application traffic, MTU ping and ten seconds of reverse UDP.
Rotation mean was 30 seconds with ±50% jitter; application-idle keepalive remained
15 seconds with ±20% jitter. Inner MTU was 1200 and UDP application payload 1000
bytes. No keepalive reduction was used to obtain this result.

| Measurement | Final result |
| --- | ---: |
| Paced active upload | 600 s requested; 4.948 Mbit/s received |
| Upload receiver loss | 3816/375001 (1.018%) |
| Receiver intervals with data | 601/601; conservative stall bound 2.000 s |
| Loaded ping | 2970/3000 |
| Application-idle duration | 600.001 s |
| Base fallbacks, active / idle / entire runtime | 0 / 0 / 0 |
| Post-idle 1200-byte inner MTU ping | 20/20 |
| Reverse UDP | 10 s requested; 5.000 Mbit/s, 0.000% loss |
| Validated client path changes / candidate socket retries | 42 / 17 |
| Established-flow repairs | 1, on the same server port |
| Captured control/small-data packets / kernel capture drops | 6427 / 0 |

The 117 interior idle samples span 588.353 seconds. Application
packet and plaintext-byte counters did not advance. Control sends/receives
advanced by 95/101, validated switches by 21, and candidate retries by
10, with zero base fallbacks. One missing PING (sequence 76) triggered a
same-port repair: replacement probes 78–80 had server-captured replies but no
client receipt; probe 81 completed, followed by registration PING 82 on the same
numbered server port. The TUN and client session remained alive.

Across the whole run, the server accepted all 93 candidate PROBEs and captured
their replies. The client received 42; the other 51 correspond to three unanswered
challenges on each of 17 replaced candidate sockets. This validates recovery
rather than assuming that a fresh socket always succeeds. The single client
session lasted 1225.570 seconds. Server and client receive-buffer-drop counters
were zero. The server's total rotation count includes its shutdown boundary;
client path changes above count confirmed migrations/repair within the session.

The active receiver's interval byte sum exactly matches its final received-byte
total. Receiver intervals are parsed from the server's text output, with 0.01 s
boundary precision. These interval bounds are conservative, not packet-level
arrival measurements. The final server capture includes task UDP ports and
packets below 300 bytes; full-size data loss comes from the receiver report.
Fingerprint correlation does not require synchronized peer wall clocks.

## Regression validation

- 22 portable library and four Linux runtime tests passed, including PROBE MAC/
  codec coverage, retained routing, same-port registration, descriptor transfer,
  malformed-descriptor cleanup and pending-advertisement ordering.
- Warning-denied clippy and the offline locked release build passed.
- Existing isolated integration passed 100 MiB each way, idle rotation, peer
  restarts and UDP loss/duplication/reordering/corruption.
- Candidate-loss regression dropped four replies, forced a replacement socket,
  received all 650 pings, and kept every UDP receiver interval nonempty with no
  base fallbacks.
- Established-flow-loss regression repaired a fixed server port with no base
  fallback and a conservative receiver stall bound of 4.502 seconds.
- The final two-client masquerade matrix passed transfers across rotations,
  lost announcements/secrets, conntrack rebinding, blocked-port base fallback and
  live server restart. Each client received at least 100 MiB each way; the largest
  conservative transfer stall bound was 1.008 seconds.
- Five control-analysis and four cross-host reporting fixtures passed, including
  the complete endurance/idle/post-check orchestration with fake CLI processes.

## Preserved attempts and limits

The original Z4 failure with 49 idle fallbacks and the failed five-second
keepalive attempt remain unchanged historical evidence. The baseline reproduced
here received 0/3 startup and 0/5 post-idle MTU pings. A preliminary candidate
implementation passed a bounded 180-second idle diagnostic, but was not treated
as full acceptance.

The first partial active attempt recorded a base fallback at 229 seconds after
PING sequences 29–31 were absent from server capture/runtime. It was stopped
after roughly 315 seconds and motivated established-flow repair. A subsequent
ten-minute active run had zero fallbacks, 4.985 Mbit/s and 0.267% UDP loss, but
the reporting harness then failed before idle because text interval boundaries
overwrote its final-summary variable. That parser was fixed and regression-tested
before repeating the complete sequence. A supplemental socket cohort that missed
its responder lifetime was excluded. These attempts are preserved separately
and never merged into the final sample.

This is one final public-path acceptance sample. It does not identify an exact
NAT/filtering device or universal timeout, establish production readiness, or
support a causal throughput comparison with earlier paths/times. Client-interface
capture was unavailable; server transmission alone cannot locate return loss.
The accepted experimental data security properties are unchanged.

## Cleanup

Only observed-source-/32 runtime UDP rules and the temporary TUN's runtime
trusted-interface association were added. Runtime and permanent zones/policies,
links, routes and forwarding matched their pre-run snapshots. Independent checks
verified all eight deployments' staging/process/interface removal and all 72 test
UDP ports free. Local task-process absence was checked. All private local keys,
builds, logs and captures were removed after sanitization; no deployment or
cleanup obligations remain.
