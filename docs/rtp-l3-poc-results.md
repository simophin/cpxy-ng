# RTP-framed L3 tunnel POC results

The experimental tunnel successfully carried kernel IPv4/TCP/UDP between the
authorized hosts, including forwarding and narrowly scoped NAT to a controlled
endpoint behind the server. It is an operational L3 POC. These measurements do
**not** establish censorship resistance, valid media camouflage, or a performance
advantage over ordinary UDP. Direct TCP results are incomplete.

Source: `experiments/rtp-l3`. Endpoint-free machine-readable measurements,
per-case timings/RTT/CPU, counters and capture statistics are in
[`2026-10-24-summary.json`](../experiments/rtp-l3/results/2026-10-24-summary.json).
Directions below are from the client's perspective. Raw evidence remains private
outside the checkout; all keys and remote staging were removed.

## TCP goodput and process cost

Three sequential 20-second repetitions per tunnel case. Rates are receiver
application goodput; CPU is median percent of **one core**, for the tunnel process
alone. The direct baseline completed one single-stream upload at 44.38 Mbit/s;
its next case exhausted two connection attempts, so a matched TCP ratio is not
available.

| Tunnel direction / streams | Median Mbit/s | Repetition range Mbit/s | Client CPU | Server CPU |
| --- | ---: | ---: | ---: | ---: |
| Upload / 1 | 40.74 | 34.14–43.65 | 22.7% | 15.0% |
| Upload / 4 | 44.69 | 13.82–45.31 | 24.6% | 15.7% |
| Download / 1 | 34.71 | 32.82–37.59 | 16.6% | 10.9% |
| Download / 4 | 123.46 | 114.71–126.56 | 32.7% | 37.1% |

The 13.82 Mbit/s upload is retained as an outlier. Client tunnel CPU across the
primary matrix was 6.58–33.52% of one core; server CPU was 2.10–37.98%. Observed
peak RSS during cases was 2,560,000 bytes client and 2,617,344 bytes server.
Server CPU uses one-second samples within each case; client CPU uses process
counter deltas across the case. iperf's own CPU fields are separate in the JSON.

## Paced UDP

One ten-second sample per rate, 1000-byte iperf payloads, increasing cautiously
and stopping each direction above 2% final receiver loss. Receiver rates include
iperf's accounting interval, so zero-loss rates are slightly below offered rates.
These are receiver loss/jitter observations, not localized diagnoses of where a
packet was lost.

| Primary tunnel | Offered Mbit/s | Received Mbit/s | Loss | Jitter ms |
| --- | ---: | ---: | ---: | ---: |
| Upload | 5 | 4.91 | 0% | 0.196 |
| Upload | 10 | 9.82 | 0% | 0.171 |
| Upload | 20 | 19.65 | 0% | 0.103 |
| Upload | 50 | 49.13 | 0% | 0.123 |
| Upload | 100 | 50.33 | 48.77% | 0.079 |
| Download | 5 | 4.83 | 3.43% | 0.082 |

The upload ceiling and download loss are the clearest operational limits in this
window. The 100 Mbit/s step deliberately exposed overload; the ladder then
stopped. Download stopped at its first step. Higher download UDP capacity is
unmeasured despite higher four-stream TCP download goodput.

| Supplemental observation | Received Mbit/s | Loss | Interpretation |
| --- | ---: | ---: | --- |
| Direct upload, offered 5 Mbit/s | 4.91 | 0% | Overlapped an evidence transfer |
| Direct upload, offered 10 Mbit/s | 9.77 | 0% | No evidence-transfer overlap |
| Direct download, offered 5 Mbit/s | 4.81 | 5.89% | Clean final comparison |
| Tunnel download repeat, offered 5 Mbit/s | 4.69 | 6.03% | Overlapped an evidence transfer; potentially contaminated |

Direct upload's next rate exhausted two control-connection attempts; no higher
rates are reported. The clean direct download also lost packets, so the wrapper
has no established loss advantage. Sequential tests and a single short path do
not distinguish traffic treatment from ordinary variation or uninstrumented
endpoint/network queues. The contaminated repeat is not a validating repeat.

## RTT, counters and buffers

With 100 received idle ping samples, direct median/p95/p99 was 179/179/180 ms;
primary tunnel was 178/179/179 ms. Across primary TCP cases, per-case loaded
medians were 178–179 ms, p95 179–199 ms, and p99 179–199 ms. The earlier smoke
mean near 77 ms came from a different window and is not a simultaneous advantage.

Latency percentiles describe surviving replies. During the tunnel's 100 Mbit/s
UDP upload, only 39 of 108 loaded ping probes returned; its low surviving-reply
RTT cannot be interpreted as healthy service. The JSON retains sent/received
counts for every idle/loaded sample.

Both tunnel sockets requested 4 MiB receive buffers but obtained **425,984 bytes**
under the hosts' existing limits; host sysctls were not changed. Local socket
receive overflow is a material POC limit:

- Primary client: 6366 receive-overflow drops, all accumulated before the UDP
  phase; zero additional receive-overflow drops during UDP, zero send/TUN-write
  drops, and zero invalid/replay rejections.
- Server, all sessions including smoke: 3903 receive-overflow and 58 send drops,
  zero TUN-write drops and zero invalid/replay rejections. Of the receive drops,
  210 preceded the primary matrix and 3693 occurred in the TCP phase. Five-second
  snapshots bracketing the entire primary UDP phase showed zero additional
  receive/send/TUN/replay drops, including the 100 Mbit/s step.
- Supplemental tunnel download: zero measured tunnel socket receive/send,
  TUN-write and invalid/replay drops, though its overlapping copy still makes
  that throughput/loss result potentially contaminated.

SO_RXQ_OVFL reports cumulative socket overflow on a subsequent received datagram.
It does not instrument NIC queues or iperf's application socket. Runtime
invalid/replay is combined; independent duplicate/out-of-order metrics were not
available in the receiver JSON and must not be treated as zero.

Primary client accepted 918,859 outbound / 1,310,292 inbound IP packets, counting
TCP retransmissions and ICMP/TCP control packets. Plain IP bytes were
851,479,470 / 1,296,649,272. Corresponding estimated outer IPv4 bytes were
902,935,574 / 1,370,025,624, excluding session setup and link-layer overhead.
Each data packet adds 56 bytes: IPv4 20 + UDP 8 + RTP 12 + AEAD tag 16.
A 1200-byte inner packet therefore costs 1256 IPv4 bytes. A 1000-byte application
UDP payload costs 1084 IPv4 bytes after both UDP/IP layers and tunnel overhead.

## Observable packet shape

Filtered Ethernet captures used 80-byte snapshots, retaining full original IP
lengths and the visible RTP header. All three tcpdump runs reported **zero kernel
capture drops**; the count-limited captures ended before the full matrix ended.
Their packet timing includes host batching/offload effects and is not a precise
on-wire interarrival measurement.

| Capture window UTC on 2026-10-24 | RTP packets | Coverage / dominant IPv4 lengths |
| --- | ---: | --- |
| 08:19:22–08:30:38 | 199,940 | Smoke plus early single-stream uploads; 1256 bytes (138,998), 108 bytes (54,524) |
| 08:33:13–08:33:22 | 30,000 | Single-stream download repetitions 2/3; 1256 bytes (23,491), 108 bytes (2,596) |
| 08:35:34–08:35:40 | 30,000 | Upload UDP 50/100 Mbit/s boundary; 1084 bytes (29,902) |

No observed IPv4 data packet exceeded 1256 bytes. Large data/small TCP-ACK modes
and bursts are conspicuous: the early upload data stream's median capture gap
was about 0.001 ms; the download data stream's median was about 0.020 ms. A gap
threshold of at most 1 ms grouped as many as 8241 packets in the UDP sample.
RTP sequences rolled over normally, timestamps advanced on the 90 kHz clock,
and implicit rollover required no extra per-data-packet counter. Passive capture
sequence gaps/reorders are not authenticated receiver-loss measurements.

The visible 69/101-byte custom control messages, large/small packet modes, lack
of media pacing/padding/RTCP, and encrypted IP payload do not reproduce a codec
stream. This is RTP framing, not a claim of media-valid camouflage or standardized
SRTP. Static PSK authentication has no forward secrecy.

## Test windows, interference and reproducibility

UTC on 2026-10-24: primary tunnel 08:30:03–08:35:59; final clean direct download
08:40:05–08:40:16. The aggregate preserves each case's exact start/end epochs.
The earlier direct TCP run was partial; modes were sequential rather than
interleaved, introducing temporal confounding. No compilation or simultaneous
throughput benchmarks ran during the matrix.

Two evidence transfers were performed before the tests had all finished. Local
file creation/write times bound their approximate payload-copy windows:

- 08:36:15.49–08:36:19.78, about 22.08 MB from server to the local controller:
  overlapped supplemental direct UDP upload 5 Mbit/s only.
- 08:38:24.49–08:38:25.86, about 2.88 MB: overlapped the first portion of the
  supplemental tunnel download repeat.

Neither copy overlapped the primary tunnel matrix or the final direct download.
Both affected supplemental results remain marked as potentially contaminated.
Setup failures are separate from packet loss: one initial direct setup timeout,
two failed direct TCP matrix attempts, one initial tunnel handshake timeout,
and two failed supplemental direct upload-UDP control attempts. Causes were not
established. Bounded retries preserved failures; no broad firewall opening was
used to work around them.

Both hosts used their existing congestion-control/buffer settings (recorded in
metadata). The client used kernel TCP in an unprivileged namespace, with a host
UDP socket inherited across `unshare -Urn`. The server endpoint behind the egress
veth had no return route to the tunnel subnet; translated-source connection logs
and successful bidirectional TCP proved forwarding/NAT without a third-party
probe. Ten MTU-sized inner pings and ten forwarded pings had zero smoke-test loss.
Internet egress beyond that controlled endpoint was not tested.

For replication, see the crate README and `scripts/benchmark.py`,
`sample-process.py`, `capture-shape.py`, and `local-integration.sh`. Supply private
endpoint/key/output arguments at runtime. Protocol validation passed seven tests
on macOS/Linux, Linux release build and clippy with warnings denied, and isolated
100 MiB transfers each way across sequence rollovers. Deterministic injection
covered lost CHALLENGE/READY, data loss, reordering, duplication and tampering.
The packet parser was independently checked with a rollover/reorder/duplicate
fixture. Public timeout summaries retain only exception type and a generic reason.

## Cleanup

The server supervisor explicitly removed all owned firewalld **runtime** rules
and interface assignments, then removed task processes/listeners, TUN, veth and
egress namespace. Forwarding remained at its original setting. Client namespace
resources exited. Remote staging directories/archives and all local/remote PSKs
were deleted. The implementer compared the existing trusted interfaces with the original
baseline. A second independent agent verified empty task rules/listeners, absent
task interfaces/namespaces/processes/staging and unchanged forwarding. Key-excluding raw evidence is retained privately; no remote
cleanup obligation remains.
