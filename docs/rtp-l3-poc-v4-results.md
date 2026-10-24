# RTP/UDP L3 tunnel v4 cross-host results

Z4's ten-minute paced upload completed, but **idle retention failed**: the
15-second keepalive candidate recorded 49 base fallbacks during ten idle
minutes. A separate five-second candidate also fell back and was stopped early.
Z5's bounded comparison completed after one failed UDP setup and one retry.
Z6 cleanup is verified. There is no validated keepalive setting for this path.

The standalone v4 runtime adds one explicit comparison option: ROTATE_SECS=0
keeps the initial non-base port fixed. Registration, keys and keepalives remain
v4. Production code is untouched. Results do not establish censorship resistance.

Evidence: [sanitized aggregate](../experiments/rtp-l3/results/2026-10-25-z4-z6-summary.json).
The filename uses the local test date (2026-10-25); the UTC windows are on
2026-10-24. Endpoints, private deployment paths, keys and raw captures are omitted.

Z4 used a 30-second rotation mean with ±50% jitter, a 15-second keepalive mean
with ±20% jitter, one client socket, inner MTU 1200, and 1000-byte UDP payloads
at 5 Mbit/s. Its run-script window was 2026-10-24T14:20:09.911564+00:00 to 2026-10-24T14:40:30.972673+00:00 UTC.
Base and rotation UDP ports were opened only to the observed client UDP /32,
using temporary firewalld runtime rules with a timeout safety net. An HTTPS
source lookup initially selected a different egress address, causing an initial
registration failure; a task-port-only capture identified the actual UDP source
and the rules were replaced before the measured run.

The NAT probe used a fresh client socket for each delay and sent once to three
server ports. Each responding server socket scheduled replies from all three
ports after that idle delay; no client keepalive was sent. The required
30/60/120/180-second probes received 1/2/3/2 initial replies respectively and
zero delayed replies. Follow-up 5/10/15/20/25-second probes received
9/8/8/9/5 delayed replies out of a maximum nine. Responding destinations observed
the same external tuple for each socket; the local port was rewritten. At 25 s,
all three same-port replies and two cross-port replies arrived.

These are single samples with initial and delayed loss. Replies in a matrix can
refresh return-path state, so the nine replies are not independent timeout
trials. Mapping expiry, filtering and path loss cannot be distinguished; no
exact timeout is claimed. The 15-second candidate (maximum jittered interval
18 s) was tested rather than accepted from these probes alone.

The 600-second upload delivered 4.968 Mbit/s: 2346/375001 datagrams lost
(0.626%), with all 601 receiver intervals nonempty. The server returned text
intervals; their parsed byte total exactly matches the receiver total and their
time precision is 0.01 s. The largest interval is one second. The server
rotated while traffic continued; the active upload capture's largest packet
gap was 315.732 ms. Loaded ping received 2481/2988 replies, with RTT
median/p95/p99 183/241/241 ms. This substantial return-path loss is separate
from the upload UDP receiver loss.

The following idle window lasted 600.000 s. Across interior samples spanning
590.087 s, application transmit and receive counters did not advance, while
control sends/receives advanced by 222/84 and fallback advanced by 49.
Server capture shows PING traffic arriving and replies leaving, while client
packet rejection and receive-buffer-drop counters remained zero. This narrows
the observation to missing return delivery or control completion; it does not
identify the underlying network device or mechanism. A second session with a
five-second mean also failed: three fallbacks, final runtime 147.102 s. It was
stopped after failure and is **not** a completed ten-minute pass. That diagnostic
overlapped only the original idle phase, with no concurrent throughput runs.

Post-idle MTU ping received 17/20 replies. Ten seconds of reverse UDP delivered
4.250 Mbit/s with 14.725% loss. Recovery worked, but the idle-retention acceptance
criterion remains failed. Do not treat Z4 as a clean pass or infer a working
keepalive recommendation.

Z5 used one 20-second TCP sample per direction and stream count, and one
60-second 5 Mbit/s UDP sample per direction. Fixed mode ran 2026-10-24T14:41:04.974261+00:00 to
2026-10-24T14:44:38.574760+00:00 UTC. The first rotation attempt ran 2026-10-24T14:45:09.094093+00:00 until its failure;
the remaining-case retry ran 2026-10-24T14:50:39.548635+00:00 to 2026-10-24T14:53:36.657591+00:00 UTC. Rotation mean was
20 s with ±50% jitter. The initial rotation UDP upload failed during setup with
an iperf stream-socket error and zero UDP samples. Its two successful TCP
upload cases are retained; one server/client restart preceded the bounded
retry of the remaining four cases, which all completed.

| Case | Fixed receiver Mbit/s | Rotation receiver Mbit/s | UDP loss fixed / rotation |
| --- | ---: | ---: | ---: |
| tcp-up-p1 | 43.972 | 42.627 | — |
| tcp-up-p4 | 47.045 | 33.160 | — |
| tcp-down-p1 | 2.254 | 2.307 | — |
| tcp-down-p4 | 62.756 | 27.839 | — |
| udp-up | 4.968 | 4.924 | 0.013% / 1.219% |
| udp-down | 4.242 | 4.113 | 15.280% / 18.100% |

The fixed server recorded zero scheduled rotations, but its client had one
fallback and returned to the same rotation path. The first rotation client had
four path switches and no fallback; the retry client had ten and no fallback.
The fixed and rotation samples therefore do not isolate only scheduled port
rotation. With one sample per case, a restart, variable return-path loss and
server receive-buffer drops, these results support no causal throughput claim.

During UDP windows, maximum server-capture packet gaps were 13.156/6.259 ms
(upload/download) for fixed mode and 386.034/10.181 ms for the rotation retry.
Six runtime path events fell inside the retry UDP windows (three each way).
Within ±3 s of those events, maximum capture gaps were 229.787 ms upload and
8.408 ms download. Nearby receiver-interval loss ranged 0–3.471% upload and
15.727–16.640% download. Per-event values are in the aggregate. These broad
windows include adjacent intervals and do not attribute all their loss to a
switch. The estimated server-minus-client clock offset was 0.360 ±0.683 s;
runtime timers have whole-second precision and script startup adds a small
offset. Raw observed port transitions include reordered old/new-port packets
and are not runtime rotation counts.

All full captures contain only RTP-framed UDP for the selected task path;
no non-RTP UDP or packets exceeding the expected 1256-byte IPv4 maximum were
observed. This framing does not imply valid media. Z4/fixed/first-rotation/retry
captures recorded 386433/594605/247483/203370 packets, with zero kernel capture
drops in each. The first rotation capture's receive-filter count exceeded its
written count by two at shutdown; this is a boundary limitation. Passive packet
gaps and sequence gaps are not final receiver loss.

The base port was not control-only. Fixed mode's capture includes 51 full-size
UDP data packets returning over base during fallback, plus startup/control
traffic. The retry includes 11 full-size MTU-ping packets each way on base
before migration, plus small startup/control packets. Per-port sizes and counts
are retained in the aggregate. During steady rotated traffic, the base remained
available for bootstrap and fallback.

Both hosts used iperf 3.18. Client/server kernels were Linux
6.12.111+deb13-amd64 / 6.12.107+deb13-amd64. TCP sender/receiver congestion
algorithms were cubic/bbr for upload and bbr/cubic for download. Iperf reported initial TCP send/receive buffers of 16384/131072 bytes and
UDP buffers of 212992/212992 bytes; per-case buffer and iperf CPU metadata is in
the aggregate. Tunnel buffers request 4 MiB, subject to host limits. Whole-server
receive-buffer-drop counts were 0 for Z4, 726 fixed, 1648 first rotation, and
37 retry; they cannot be assigned to individual cases from these counters.
No new direct baseline, peak-hour claim or multi-repetition comparison is made.

Linux validation passed 18 library tests, the inherited-socket runtime test,
offline release build and clippy with warnings denied. Python syntax checks,
a synthetic capture transition/outage fixture, a clock/window/loss-correlation
fixture, and a failure-preserving continuation fixture passed. The new fixed
mode is exercised by the actual baseline, whose server reports zero rotations.

Cleanup restored exact pre-task runtime zone/policy and link snapshots, with
IPv4 forwarding unchanged and permanent firewalld configuration untouched.
Task services, client namespaces, interfaces, staging/build files and all key
copies were removed, and process absence was verified on both hosts. Unrelated
services were preserved. Local private staging and the local key were also removed.

The remaining acceptance issue is public-path idle retention. The shorter
interval diagnostic did not resolve it; future work needs targeted return-path
evidence before choosing a keepalive setting or accepting Z4.
