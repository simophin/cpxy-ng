# Packet-path counters and worker poll results — 2026-10-28

Step 1 of the [packet-path plan](rtp-packet-path-efficiency-plan.md): aggregate
worker and shim counters, plus a change to the client worker's idle poll, measured
on tr3600 against the live binary. **The counters cost nothing measurable. The
poll change cuts idle worker CPU by about 80% and does not change loaded
throughput or CPU. Inbound packets are dropped at the worker's UDP socket and at
the socketpair handoff well below saturation.**

## What changed

- `rtp-l3` client worker: cumulative `PathCounters`, reported as `path` in its
  existing 10-second `stats` event. They count polls, idle polls, packet-device and
  UDP calls, calls that would block, rejected/foreign datagrams, failed sends,
  unrouted outbound packets, and device write-queue full/stale drops with its
  high-water mark. No per-packet logging, addresses or payloads.
- `rtp-mobile` shim: per-descriptor call, would-block and failed-send counts for
  the TUN and its socketpair end. `Status` (the router's 10-second JSON line)
  now includes `worker`, `shim_tun` and `shim_pair`; the FFI status is unchanged.
- Client worker poll: it waits for TUN/socketpair writability while device writes
  are queued (previously retried by the 2 ms timeout), and otherwise times out
  after 50 ms instead of 2 ms. It keeps 2 ms while an HTTP preparation job runs,
  since that thread has no descriptor. Protocol timers are 500 ms or longer; the
  50 ms bound also limits how late the run flag and repair requests are seen.
  The endpoint loop is unchanged.
- Profiler: `/proc/net/dev` interface packet and drop counters (a TUN's `tx_drop`
  is its full device queue).

## Method

Three router binaries, all ARM64 static musl, interleaved
ref/A/B/ref/A/B/ref/A/B/ref in one session:

| Label | Binary | sha256 |
| --- | --- | --- |
| ref | live production binary (PR #155 fixes) | `fbebe095…e7ba84` |
| A | counters, old 2 ms poll | `f8485ae1…b05c1a` |
| B | counters and new poll | `d6013666…65d53` |

Each window restarted the router service, waited for the tunnel plus 15 seconds,
then ran three phases, each with a 1-second profile and concurrent tunnel pings:

- idle, 30 seconds;
- fixed load: four router-originated downloads through `rtpr0`, each limited to
  2.5 MB/s (about 81 Mbit/s total, below stable capacity), 80,000,000 bytes each;
- saturation: four unlimited downloads of 80,000,000 bytes.

The origin was a temporary HTTP server bound only to hh-hk's tunnel address,
reached through a temporary router host route; both were removed afterwards.
All 72 downloads returned HTTP 200 with exact lengths. Ranges are over three
windows (four for ref). The profiler's own shell sampling is included in
system-wide CPU, mostly visible at idle. Idle RTT alternated between about 37 and
83 ms across windows independent of the binary, an external path change, so RTT
is only compared within a window. No IRQ, offload, MTU, buffer or sysctl settings
were changed. Driver: `experiments/rtp-l3/scripts/router-windows.sh`; summary:
`experiments/rtp-l3/scripts/summarize-router-windows.py`;
[per-window results](../experiments/rtp-l3/results/2026-10-28-path-counters-poll.json).

## Results

Median (range). Core values are fractions of one core; "ms/MB" is core-milliseconds
per delivered megabyte.

| Phase | Metric | ref | A | B |
| --- | --- | ---: | ---: | ---: |
| idle | worker CPU | 0.0101 (0.0098–0.0175) | 0.0095 (0.0094–0.0104) | 0.0020 (0.0017–0.0020) |
| idle | worker voluntary switches/s | 478 (478–504) | 480 (478–481) | 22 (22–23) |
| idle | system busy cores (incl. profiler) | 0.38 (0.36–0.41) | 0.35 (0.33–0.37) | 0.35 (0.32–0.35) |
| fixed | Mbit/s | 81.5 | 81.7 | 81.4 |
| fixed | system busy ms/MB | 111 (110–116) | 112 (108–120) | 109 (108–113) |
| fixed | process ms/MB | 56.9 (55.4–58.6) | 57.3 (55.1–57.4) | 56.2 (55.1–56.9) |
| fixed | UDP receive drops/window | 17.7k (16.3–23.8k) | 19.0k (18.5–22.5k) | 16.1k (15.0–20.7k) |
| saturation | Mbit/s | 127 (124–128) | 128 (128–128) | 126 (119–128) |
| saturation | system busy ms/MB | 107 (105–109) | 102 (102–103) | 109 (103–112) |
| saturation | worker / shim cores | 0.57 / 0.35 | 0.55 / 0.34 | 0.55 / 0.34 |
| saturation | UDP receive drops/window | 22.2k (21.0–31.7k) | 24.5k (21.5–32.8k) | 22.5k (22.0–23.3k) |
| saturation | pings received of 20 | 18.5 (18–19) | 19 (18–19) | 19 (17–20) |

- **Counters (A vs ref):** every loaded metric is within the reference range.
- **Poll (B vs A):** idle worker CPU fell by about 80% and wakeups by about 95%.
  Loaded throughput, CPU per byte, drops and ping loss stayed within run-to-run
  variation. The change is an idle-efficiency fix; it does not approach the
  plan's 15% loaded gate and is not presented as a throughput gain.

## What the counters show (step 1)

From B's fixed-load windows (about 279,000 inbound and 51,000–67,000 outbound
packets per 30 seconds):

| Per inbound packet | Calls |
| --- | ---: |
| Worker UDP receive | 1.05 |
| Worker write to socketpair | 1.00 |
| Shim read from socketpair | 1.07 |
| Shim write to TUN | 1.00 |

Each inbound packet costs about four syscalls, two of them the socketpair hop.
Outbound is symmetric. The worker handles about 18 packets per poll under load.
UDP `recvmmsg` or io_uring would address one of the four.

Inbound loss happens **at 81 Mbit/s, below capacity**, while the worker uses only
about 36% of a core:

- The worker's UDP socket receive queue peaks at 3.6–3.9 MB, its full 4 MiB
  accounting limit, and the kernel drops 15–24k datagrams per window, roughly
  6% of arrivals.
- The worker's write queue toward the shim reaches its 218-packet cap in every
  loaded window and drops 1.9–4.3k packets. Its socketpair writes would block
  540–1,050 times per window; the shim's TUN writes never block.
- `net.unix.max_dgram_qlen` on the router is **10**: a connected Unix datagram
  socketpair holds at most ten queued datagrams, regardless of the 1 MiB
  buffers requested. A brief shim scheduling delay therefore backs up the worker.
- The TUN's `tx_drop` rose by 0.2–2k per loaded window: outbound packets
  (mostly ACKs) dropped while the shim's TUN reader lagged.

So loss comes from short stalls along a single-file handoff chain, not sustained
CPU exhaustion. That points to step 3 (removing the socketpair hop) ahead of UDP
batching or io_uring. A per-packet CPU split between kernel stages still needs
sampled profiling, which this router kernel does not support: it has no perf
events or tracefs.

## Not covered

Upload and bidirectional load, one/eight streams, forwarded LAN/Wi-Fi traffic,
endpoint multi-client behavior, and kernel-stage attribution. The router was
left on the reference binary.

## Validation

- `rtp-l3` 26, `rtp-mobile` 18 and `rtp-service` 26 library tests pass,
  including a new poll test (input, pending writes, timeout) and stats parsing.
- The production five-minute rotation regression passes with the new poll
  (311.12 seconds).
- Profiler parser tests pass, including the interface counters.
- Rollback copy of the live router binary:
  `/root/cpxy-rtp-router-before-counters-20261028` (identical to the reference).
