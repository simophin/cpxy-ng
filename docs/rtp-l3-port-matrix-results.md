# Controlled immediate destination-port diagnosis

The bounded public-path matrix received all 192 replies from four server ports
using one fresh local UDP socket. All destinations produced the same observed
external client tuple. This sample does not reproduce the tunnel's missing
return packets, identify their cause, or complete Z4 idle retention.

Evidence: [sanitized matrix](../experiments/rtp-l3/results/2026-10-25-port-matrix-summary.json).
The probe implementation was committed as `7d37835`. Both peers used that
Python script; no tunnel settings or implementation changed.

## Method and observations

The client ran 12 rounds, shuffling all four destinations in each round with
seed 25. It waited 0.5 seconds per request before moving on, for 48 trials and
about 24 seconds of probing. Every request carried a fresh correlation token;
late replies from previous trials were excluded. Each request caused the server
to send one immediate reply from each bound socket to the observed client tuple.
The server was bounded to 100 seconds and its temporary firewall rules to 150.

| Observation | Result |
| --- | ---: |
| Requests received by server | 48/48 |
| Successful server reply send calls | 192/192 |
| Replies from contacted source port | 48/48 |
| Replies from other source ports | 144/144 |
| Distinct observed client mappings | 1 |
| Client request-to-reply times | 83.070–95.818 ms |

The first destination was numbered port 1. Replies from all four ports arrived
on that first request, before the client sent a request to the other three.
Within this sample, different destination ports on the same server IP did not
produce different mappings, and replies from initially uncontacted server ports
could reach the socket. No ordinary loss was observed in these immediate probes.
Server evidence is successful `sendto` calls and private observed tuples, with
client socket reception; neither host had an interface capture in this run.

## Limits and next experiment

The payload is plain JSON UDP rather than the tunnel's encrypted RTP framing.
Ports, timing and path conditions differ from the earlier tunnel sample.
Four destinations on one server IP do not characterize mapping across IPs.
A fresh bound socket does not prove that upstream devices had no preexisting
state for its externally selected tuple. Cross-port replies themselves can
refresh filtering state. This is an immediate-delivery baseline, not an idle
retention test or proof of a particular NAT implementation.

The next controlled experiment should interleave matched immediate controls
with delayed same-source and cross-source replies on independent fresh sockets,
repeat each delay/destination, and correlate server sends with client-interface
arrival evidence where available. Separate silent sockets from sockets receiving
control traffic so controls do not refresh the mappings being timed. Then compare
with traced tunnel packets under matched timing/shape before changing socket or
keepalive strategy. Z4 and the cause-isolation TODO remain open.

## Validation and cleanup

The real loopback fixture verified all-source delivery, per-trial correlation
and endpoint-free output. CLI fixtures rejected duplicate, missing and invalid
ports. Both tests passed with socket access enabled; the sandbox itself forbids
UDP sockets. No Rust runtime code changed.

Only source-/32-restricted runtime firewalld rules were added; they were removed
in the remote supervisor's `finally` block. Runtime zone configuration and links
exactly matched pre-run snapshots. A subsequent check bound all four test ports
and verified remote staging absence. Server/client probe processes exited,
private server logs and staging were removed, and no keys or TUNs were created.
