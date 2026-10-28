# Async client and endpoint results — 2026-10-28

The client worker and the endpoint now run on Tokio, and a pair of bounded channels has
replaced the router's socketpair (see the [packet-path plan](rtp-packet-path-efficiency-plan.md)).
Both are deployed: hh-hk runs the async endpoint and tr3600 runs the async client for the
hybrid network. **The router's CPU per delivered byte fell by about 20%, and throughput and
latency are about unchanged. Inbound loss under load did not improve: the shim writing to
the TUN is now the limit.**

## Deployed binaries and rollback

| Host | Path | sha256 | Rollback copy |
| --- | --- | --- | --- |
| hh-hk | `/usr/local/bin/cpxy-rtp` | `d30789d4…e18c7` | `/root/cpxy-rtp-before-async-20261028` (`fb2d4e4c…`) |
| tr3600 | `/usr/bin/cpxy-rtp-router` | `2089407c…6b42` | `/root/cpxy-rtp-router-before-async-20261028` (`fbebe095…`) |

To roll back, stop each service, copy the rollback file over the binary, and start the
endpoint before the router. Configuration, firewall policy and MTU are unchanged. After
an endpoint restart, the router client reconnects on its own (observed: repairing,
degraded, connected).

## Router measurements

All measurements use the same method as [the counters results](rtp-path-counters-results.md):
router-originated downloads through `rtpr0` from a temporary origin on the endpoint's
tunnel address, at a fixed 81 Mbit/s and at saturation, with tunnel pings. Medians are
over three windows. A and B were interleaved with the reference in one session. The
deployed rows come from later windows, against the same path but not interleaved.

| Variant | Process ms/MB (sat) | Router busy ms/MB (sat) | Mbit/s (sat) | Ping max (fixed / sat) | Inbound loss (fixed / sat) |
| --- | ---: | ---: | ---: | ---: | ---: |
| A: previous sync worker, socketpair | 56.3 | 105 | 128 | 118 / 146 ms | 5.7% / 8.4% |
| B: async worker, 1024-packet device queue | 46.6 | 96.5 | 128 | 133 / 186 ms | 7.9% / 7.7% |
| Deployed, 256-packet queue | 53.0 | 104 | 117 | 90 / 121 ms | 13.2% / 12.4% |
| **Deployed, 256 + backpressure** | **45.2** | **91.7** | **128** | 108 / 158 ms | 7.3% / 10.6% |

Inbound loss is kernel UDP receive drops plus worker device-queue drops, as a share of
arrivals (`experiments/rtp-l3/scripts/inbound-loss.py`). It counts drops of TCP's own
bursts. Goodput held at the offered rate in every fixed-load window.

What the variants showed:

- **B** cut CPU, but most of its fall in socket drops reappeared as device-queue drops. A
  full 1024-packet queue is about 75 ms of data, and loaded ping maxima rose.
- **256 alone** bounded latency, but the worker kept receiving and decrypting datagrams it
  could not queue, then dropped them. Loss doubled and saturation throughput fell.
- **Backpressure** makes the worker stop receiving while the device queue is full, so
  bursts wait in the kernel socket buffer as they did before. The worker never decrypts a
  packet it then drops (device-queue drops are 0). CPU is the lowest of any variant, and
  throughput is back to 128 Mbit/s.

On the endpoint, the async server used about 50–55 core-ms per delivered MB. That figure
is absolute: there are no interleaved reference windows for the endpoint, because it was
replaced directly. Its device queue dropped nothing.

## What limits it now

Every remaining inbound drop is at the router's UDP socket while the worker waits on a
full device queue. The consumer of that queue, the shim writing each packet to the TUN,
cannot keep up with the endpoint's bursts. A TUN write runs the kernel's receive path in
the writer's thread. The next experiment is on that consumer: write batching or a
dedicated writer thread for the TUN. Socket buffer size and worker changes are not the
next step.

## Validation

- `rtp-l3` 26, `rtp-mobile` 18 and `rtp-service` 26 library tests pass. The service lab
  passed 15 repeated runs.
- The production five-minute rotation regression passes on the async client (311.88 s)
  and on the async client and endpoint together (311.29 s).
- The `linux-internet.sh` namespace fixture on hh-hk passes with the async binaries
  (NAT, TCP/UDP, router bypass, crash guard: `INTERNET_EXIT_OK`).
- Router smoke tests pass for the tunnel and split DNS, including queries injected
  through the tunnel.
- A lab test for backpressure was attempted and dropped. Lab buffering depends on the
  host (`net.unix.max_dgram_qlen` is 512 there and 10 on the router), and the margin
  between failing with and without backpressure was too small to give a stable test.
