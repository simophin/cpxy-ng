# Public-path control completion diagnosis

The next handoff item is complete: matching PING/STATE traces now distinguish
missing return delivery from decoder/state rejection. This bounded run reproduced
three fallbacks. It does not complete Z4 or identify a NAT timeout or faulty device.

Evidence: [sanitized aggregate](../experiments/rtp-l3/results/2026-10-25-control-trace-summary.json).
The runtime wire format, sequence acceptance, keepalive and port strategy were
unchanged. Both peers used the consolidated protocol. Opt-in traces fingerprint
the encrypted UDP payload with SHA-256 and record monotonic/wall microseconds,
control metadata, decoder outcome and state acceptance without endpoints or keys.
The analyzer joins each fresh PING to the server reply and client outcome, and
matches fingerprints against a private full-payload server Ethernet capture.
Matching does not depend on clock synchronization. The stable-offset consistency
bounds for server minus client clocks were -21.110 to +156.326 ms; these are not
one-way-delay measurements.

The single-client run kept one local socket, a 15-second keepalive mean, a
30-second rotation mean and inner MTU 1200. The run script sent three startup
pings, slept 180 seconds, then attempted five MTU pings. Client runtime was
188.400 seconds; startup ping received 3/3, post-idle MTU ping 0/5. No throughput
or reverse UDP acceptance run was attempted. The short diagnostic preserves a
failure and is not the ten-minute idle/endurance acceptance matrix.

Of 32 fresh PINGs, 31 reached the server runtime and were accepted. Sequence 9
was absent from both the server capture and runtime. Every accepted PING
produced a solicited STATE and a successful socket send; all 31 solicited STATEs
appear in the server capture. Only 15 reached the client runtime, and all 15
decoded and were accepted. The other 16 had no matching client receive event.
There were no received-but-superseded solicited echoes in this sample.

Three exhausted PING groups caused fallback: sequences 17–19, 22–24 and 26–28.
All nine replies were captured at the server and none reached the client runtime.
The client recorded zero decoder rejections and receive-buffer drops. Base
fallback sequences 20, 25 and 29 all completed; the newly advertised rotation
path subsequently failed again for the latter two. Numbered per-port counts and
per-sequence lifecycles are in the aggregate. One earlier numeric rotation port
worked and later failed; another had nine solicited replies with no client
receipt. These observations motivate a controlled destination-port experiment,
but time, port epoch and path conditions were not independently varied here.

Two received STATEs were ignored with `instance_requires_fresh_echo`: both were
unsolicited bootstrap announcements with echo zero, before the solicited retry
established the server instance. They were correctly ignored and do not explain
the later fallbacks. A regression test separately verifies that a superseded
echo is ignored while the current echo completes without producing a new PING.

The server sent 47 STATEs including announcements/retries; 21 reached the client,
19 were accepted and the two bootstrap announcements were ignored. Of the 26
unmatched sends, two final announcements occurred after client exit and cannot
be counted as path loss. The capture contains 92 packets, versus 93 received by
the capture filter, with zero reported kernel capture drops. Its final boundary
omits one server announcement. All solicited replies and the nine fallback
replies were captured, so this boundary does not affect the fallback finding.
Runtime send success establishes kernel acceptance; server-interface capture
adds transmission observation but cannot locate loss between that interface
and the client runtime. There was no client host-interface capture. Client-side
filtering/socket delivery and network loss remain unresolved alternatives.

Validation passed 19 portable tests, 20 Linux tests including inherited sockets,
warning-denied Linux clippy, offline release build and four analyzer fixtures.
The final build passed traced namespace regression with 100 MiB each way,
idle rotation, restarts and bounded UDP faults. An initial namespace attempt
completed its traffic but failed a harness assumption that `ready` is the first
log event; optional startup traces now precede it. Both subsequent regressions
passed. Public measurement preceded only the final trace-enable caching/lazy
JSON optimization; the event schema and protocol/state behavior are unchanged.

Server runtime zones/policies, links and forwarding exactly matched pre-run
snapshots after cleanup; permanent firewall configuration and unrelated services
were preserved. Task processes, TUNs, namespaces, private staging/builds, captures
and all key copies were removed after recording sanitized evidence.

The next task is to isolate destination-port dependence and NAT mapping/filtering
from ordinary loss, with matched controlled probes and client-side arrival
evidence if available. Do not change keepalive or socket strategy from this
single sample. Z4 remains failed/open.
