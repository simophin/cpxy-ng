# RTP router throughput investigation — 2026-10-28

The slowdown was reproducible on `root@tr3600`. The original hh-hk CPXY HTTP
transport provided a substantially faster baseline than the deployed RTP v2
transport. Receive-buffer overflow and packet-processing overhead contributed;
the evidence does not support an MTU mismatch as the cause.

The changes in this investigation are live on the router and hh-hk, with the
original binaries retained for rollback. They improve throughput but do not
close the gap to the old proxy or eliminate overload loss.

## Measurements

All rates below are application goodput in decimal Mbit/s, including connection
startup. Each stream receives exactly 25,000,000 bytes. Four-stream goodput is
total bytes divided by the full concurrent phase duration, **not** the sum of
individual curl rates. Medians and ranges refer to adjacent short runs.

| Private hh-hk origin | Old CPXY HTTP proxy, four streams | RTP, one stream | RTP, four streams |
| --- | ---: | ---: | ---: |
| Final changes, before original-binary retest (3 rounds) | 316 (300–321) | 94 (65–109) | 131 (127–134) |
| Original deployed binaries retested (2 rounds) | 306 (295–316) | 55 (52–59) | 62 (61–64) |
| Final changes restored (3 rounds) | 321 (285–323) | 85 (78–85) | 116 (116–120) |

This final/original/final sequence supports a real improvement beyond changes
in peak-hour capacity: four-stream proxy capacity stayed around 300 Mbit/s while
RTP fell to about 62 on the original binaries and recovered on restoration.
It is not a randomized benchmark, and live user traffic remains a confounder.
Short single-stream proxy runs varied substantially (roughly 75–175 Mbit/s across
these windows), making the concurrent baseline more useful for path capacity.

Initial public HTTPS downloads from Cloudflare gave **66–75 Mbit/s through the
old hh-hk proxy versus about 40 Mbit/s through original RTP**. After polling and
buffer changes, two valid public RTP samples reached 76 and 83 Mbit/s. Cloudflare
then returned HTTP 429, so subsequent public attempts were excluded. Oversized
initial requests returned HTTP 403 and were also excluded. One initial RTP sample
overlapped another benchmark and is excluded from the clean comparison.

Longer private-origin transfers, four streams of 100,000,000 bytes, completed at
about **132 and 153 Mbit/s over final RTP**, versus **328 Mbit/s over the proxy**
in a nearby window. These are individual windows, not repeatability claims.

The historical [random-nonce report](rtp-l3-random-nonce-results.md) recorded
about 36 Mbit/s for one download stream and 130+ for four streams on its test
path. That makes a remembered 100 Mbit/s result plausible, but this investigation
does not reproduce the previous RTP version on the same path and cannot assign
a precise v1-to-v2 regression percentage.

## Method and limits

- Physical router: two Cortex-A53 cores at 2 GHz, OpenWrt Linux 6.12.94.
- A temporary CPXY client on router loopback port 15000 targeted hh-hk's old
  HTTP transport. The retained production client targets Melbourne and was not
  used for the golden baseline. Credentials stayed in the temporary process
  environment, outside command arguments and retained artifacts.
- Router curl used either that HTTP proxy or `--interface rtpr0`. Router-originated
  traffic normally bypasses selected-LAN routing, so explicit binding matters.
- A temporary HTTP origin bound only to hh-hk's private tunnel address supplied
  both transport paths. A temporary host route made that address reachable
  through the router TUN. The origin, route and temporary proxy were removed.
- RTP used the configured IPv6 outer path. The legacy proxy used hh-hk's public
  hostname; its ingress was observed over IPv6. Only IPv4 inner downloads were
  measured. Origin-side TCP behavior differs between a TCP-terminating proxy
  and a packet VPN, so the proxy is a practical capacity baseline, not identical
  end-to-end congestion control.
- These are router-originated transfers, not Wi-Fi or forwarded-LAN acceptance.
  QEMU cannot establish this hardware's throughput, so the physical router was
  used. No claim of a fixed attainable Internet speed is made.

[Sanitized measurements, counters and hashes](../experiments/rtp-l3/results/2026-10-28-router-throughput.json)
contain each controlled phase. The portable
[download fixture](../experiments/rtp-l3/scripts/router-download-bench.sh) repeats
alternating one/four-stream tests after an operator provisions the origin and
loopback proxy. It changes no routing, services, credentials or firewall rules:

```sh
ssh root@tr3600 'BENCH_URL=http://PRIVATE_ORIGIN:PORT/ BENCH_ROUNDS=3 sh -s' \
  < experiments/rtp-l3/scripts/router-download-bench.sh
```

Use a controlled origin with a known response size and discard incorrect
lengths. The script exits on HTTP/transport failures; rate-limit responses are
not bandwidth samples.

## Findings and changes

1. **UDP receive buffering.** The router started with a 212,992-byte receive
   default. Receive-buffer errors rose during downloads. Temporarily increasing
   the default to 4 MiB improved single-download speed to about 67 Mbit/s and
   removed drops in those short samples. The global defaults were subsequently
   restored. The fix requests 2 MiB per RTP receive socket (4 MiB kernel accounting
   on Linux), including repaired sockets and new server leases. Privileged Linux
   hosts use `SO_RCVBUFFORCE` to bypass a small host ceiling; unprivileged and
   mobile hosts keep the ordinary capped `SO_RCVBUF` fallback. No new privilege
   requirement or host sysctl change is introduced. This follows Linux's
   [socket buffer accounting](https://man7.org/linux/man-pages/man7/socket.7.html).
2. **Packet-loop scheduling.** Both loops processed bounded 32-packet batches
   and then slept 2 ms even with queued traffic. They now poll TUN/socketpair
   and allocation sockets for readability, waking immediately for pending
   data while retaining a 2 ms idle deadline for control timers. Batch limits,
   ownership and queue limits remain bounded.
3. **Replay-window CPU cost.** Advancing the 1024-bit replay window previously
   examined individual bits. It now shifts at most 16 machine words, preserving
   the window and replay behavior. A packet-history regression covers word
   boundaries, large jumps, gaps and reordered packets. Adjacent live samples
   do not isolate a reliable throughput percentage for this optimization alone.

## MTU and residual limits

Both TUNs have MTU 1200. Captured benchmark SYNs advertise MSS 1160 in both
directions; timestamped TCP payloads are 1148 bytes. Four full-size inner pings
(1172-byte ICMP payload, 1200-byte IPv4 packet) succeeded. The router's IPv6
fragment-failure counter stayed at 5957 throughout the tests. MTU was not changed.

During a sustained final four-stream run:

| Observation | Result |
| --- | --- |
| Physical core busy time | core 0 about 92%; core 1 about 87% |
| RTP packet worker CPU | about 63% of one core |
| Two packet-shim runtime threads combined | about 41% of one core |
| System-wide CPU time | about 27% user, 55% system, 8% IRQ/softirq |
| Router UDPv6 receive-buffer errors | +22,698 over 20.95 seconds |
| Idle tunnel ping | 5/5; average 39.4 ms, maximum 41.2 ms |
| Loaded tunnel ping | 18/20; average 79.4 ms, maximum 159.6 ms |

Process and per-thread CPU include both user and charged kernel time; system-wide
CPU also includes unrelated router work. Host-wide drops are not a per-allocation
counter, although the observed rises track RTP phases and not proxy phases.
The endpoint reported a 4 MiB socket receive limit. Larger buffers absorb bursts;
they do not solve sustained processing overload or guarantee low loaded latency.

The router already uses both physical cores. Its data path has a single RTP
worker plus two Tokio shim threads, with a datagram socketpair between the TUN
shim and transport worker. More threads alone are not supported as the next
optimization by these measurements. Batching and reducing copies/handoffs merit
profiling before adding flow-sharding and lifecycle complexity.

## Validation and deployment

- Final library suite: 20 rtp-l3, 18 rtp-mobile and 26 rtp-service tests passed.
- Production five-minute rotation regression passed separately in 311 seconds,
  including bidirectional traffic and old-lease retirement. This ran after
  polling/buffering changes and before the replay-only change; the final regular
  suite, including replay/repair tests, was rerun afterward.
- Final endpoint binary passed `linux-internet.sh` in private network/mount
  namespaces on hh-hk: NAT, TCP/UDP, router bypass and crash guards
  (`INTERNET_EXIT_OK`). The local host cannot create a TUN; its socket-based
  library tests ran with host socket access.
- Release ARM64 musl router/service and x86-64 musl service builds passed.
- Stable 1.99 Clippy completed with five existing warnings in unchanged logic.
  The pinned 1.97 toolchain has no installed Clippy component.
- Benchmark shell syntax and `git diff --check` passed.

The live router executable is `/usr/bin/cpxy-rtp-router`; hh-hk uses
`/usr/local/bin/cpxy-rtp`. The changed binaries persist across restart; the
installed OpenWrt package's version/manifest was not changed by this binary
hotfix, so a future package replacement must include these fixes.

Original rollback copies:

- tr3600: `/root/cpxy-rtp-router-before-throughput-20261028`
- hh-hk: `/root/cpxy-rtp-before-throughput-20261028`

Restore each original executable while its service is stopped, then start the
endpoint before restarting the router. The original global router receive-buffer
default and maximum are both restored to 212992. Configuration, MTU and firewall
policy were preserved. Final executable hashes are in the JSON evidence and host
deployment notes.
