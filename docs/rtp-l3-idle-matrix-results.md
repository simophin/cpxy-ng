# Independent silent-socket and destination-port diagnosis

The current public path showed destination-port-dependent external mappings and
reply delivery. One local socket produced four stable external UDP ports, one
per destination. Same-source replies arrived after 0.1–15 seconds of silence;
none arrived after 30–180 seconds. Cross-source replies were absent at every
tested delay. All immediate controls on separate sockets arrived.

These observations establish a repeatable distinction from indiscriminate loss
in these samples. They do not identify the translating/filtering device, locate
return loss, establish an exact timeout, explain the tunnel's failure on their
own, or complete Z4. No tunnel code, socket strategy or keepalive setting changed.

Evidence: [endpoint-free summary](../experiments/rtp-l3/results/2026-10-25-idle-matrix-summary.json).
The earlier [immediate baseline](rtp-l3-port-matrix-results.md) remains unchanged:
it observed one mapping and 192/192 replies. The current sample differs; neither
measurement should be generalized across paths or time.

## Delayed experiment

The main run used `idle-matrix.py` from `d04ef29` on both peers. Four server
ports, eight delays, two reply-source cases and two repetitions produced 128
trials, randomized with seed 25 and 0.05-second launch spacing. Every trial used
three distinct local sockets: one delayed probe, an immediate start control and
an immediate deadline control. All sockets stayed open until the run ended.
Only the single request and scheduled reply touched each silent socket; no
immediate acknowledgement, retries or control traffic refreshed it.

Requests/replies were matched 192-byte JSON UDP payloads (220 outer IPv4 bytes),
without RTP framing or tunnel encryption. Each receive window extended two
seconds beyond its requested delay. The main client ran for 188.155 seconds;
the server lifetime was bounded to 210 seconds. Successful server send calls
were joined to exact UDP-payload fingerprints in the server-interface capture.

| Delay, seconds | Same-source replies | Cross-source replies |
| ---: | ---: | ---: |
| 0.1 | 8/8 | 0/8 |
| 1 | 8/8 | 0/8 |
| 5 | 8/8 | 0/8 |
| 15 | 8/8 | 0/8 |
| 30 | 0/8 | 0/8 |
| 60 | 0/8 | 0/8 |
| 120 | 0/8 | 0/8 |
| 180 | 0/8 | 0/8 |

All 256 immediate controls arrived, with request/reply times of
78.762–244.899 ms. The server received and sent all 384 legs; all 384 requests
and 384 replies appeared in its capture. Its scheduler exceeded requested
delays by only 0.081–1.820 ms. The 384 server-observed mappings belong to
distinct local sockets and do not themselves characterize destination dependence.

## Socket-receive companion

The initial client logged accepted replies. A bounded overlapping companion
used client receive instrumentation from `80f56bc` against the same server
build, with unchanged wire format and scheduling. It logged every socket
receive before source/token/body checks, distinguishing absence from rejection.
It covered 0.1, 1, 5, 15 and 30 seconds with otherwise matching settings:
80 trials/240 sockets over 35.804 seconds.

Each same-source delay through 15 seconds again delivered 8/8; at 30 seconds
it delivered 0/8. All 40 cross-source replies were absent. All 160 controls
arrived. The client recorded 192 datagrams, accepted all 192, and rejected none;
the other 48 legs recorded zero socket receives. All 240 requests and replies
appeared in server logs and capture. Together the main and companion runs
produced 1248 captured packets, with zero reported kernel capture drops.
The companion overlaps the main run and is not an independent day/path sample.

## Immediate contact-history comparison

After delayed-run cleanup, the existing `port-matrix.py` from `7d37835` ran a
separate bounded four-port comparison: one fresh local socket, 12 randomized
rounds, seed 25, 0.5-second windows and a 40-second server lifetime. Each of
48 requests caused immediate replies from all four server ports. This uses
the original variable-size plain JSON payloads, not the delayed probe's fixed
192-byte payload. No local socket rotation or reconnect occurred.

| Observation | Result |
| --- | ---: |
| Requests received / captured | 48/48 |
| Successful server reply sends / captured replies | 192/192 |
| Same-source replies received | 48/48 |
| Cross-source replies from previously contacted ports | 0/138 |
| Cross-source replies from uncontacted ports | 0/6 |
| Observed external client addresses | 1 |
| Observed external client UDP ports | 4 |
| Mappings per destination | 1 stable mapping each |
| Kernel capture drops | 0 |

The four external ports were distinct even though the client retained one
local UDP socket. Each cross-source reply was sent to the tuple observed for
that request's destination; prior contact with its source port did not make
it arrive through that destination's mapping. This supports destination-port
dependence of mapping and source-port dependence of return delivery on the
current path. It does not identify a specific NAT implementation or prove
where filtering/loss occurred. Four ports on one server IP do not establish
mapping behavior across different destination IPs.

## Excluded attempts and limits

The first setup used a public address reported by an HTTPS service, but server
capture showed that UDP used a different source address. The source-restricted
firewall rules therefore blocked requests. It was stopped, cleanup verified,
and the experiment retried using the observed UDP source /32. Its saved progress
record contains 352 client sends over 66.201 seconds and zero accepted replies;
it is a failed setup, not a valid return-loss measurement.

A supplemental contact-history check sent 48 requests after the bounded
responder had stopped and its rules/staging had been removed. None reached the
responder. This invalid attempt is preserved separately and superseded by the
bounded immediate comparison above; its zeros are excluded from all results.

Client-interface capture was unavailable: this unprivileged client has neither
capture tools nor capture capabilities. No client sudo, package installation,
host routes or sysctl changes were used. Server capture and zero capture drops
do not establish downstream delivery. The companion rules out client decoder
rejection for its missing replies, but cannot locate loss before the socket.

The 15-second delivery / 30-second failure brackets observed behavior under
these settings; it is not an exact NAT timeout or a keepalive recommendation.
Actual PING/STATE packets and the inherited tunnel socket still require matched
tracing. In particular, the earlier five-second keepalive failure is not
explained merely by observing these plain-UDP timing results. Z4 remains open.

## Validation and cleanup

Five delayed-probe fixtures pass, covering real delayed delivery, distinct
silent/control sockets, missing replies, rejected-token datagrams, private-file
creation/CLI bounds, capture joins and sanitization. Both existing immediate
matrix fixtures pass. Rust/tunnel runtime code was untouched.

Only source-/32-restricted runtime firewall rules were added, with timeouts and
explicit removal. After each server deployment, runtime and permanent zones
and policies, links and forwarding exactly matched their snapshots. Task
processes exited, all test ports rebound successfully, and remote staging was
removed. No TUNs or keys were created. Private local staging/logs/captures were
removed after publishing sanitized evidence; no deployment remains.

The next step is to compare traced tunnel PING/STATE behavior with these
controlled mapping, source-port and idle observations under matched conditions,
obtaining client-interface evidence if available. Establish the tunnel-specific
cause before changing socket/port strategy or keepalive, then repeat Z4.
