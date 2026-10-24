# Historical 24-byte random nonce results

Historical measurement record. The current design is specified in
[the protocol specification](rtp-l3-protocol.md). Artifact filenames and recorded
protocol identifiers are frozen evidence, not supported runtime modes. These
measurements predate the unified key labels/reserved-zero control header and
do not validate those new wire conventions.

Random-nonce replaces AEAD's PSK handshake and ChaCha20-Poly1305 with handshake-free raw
XChaCha20 and a fresh random 24-byte nonce per packet (conventions in
`docs/rtp-l3-poc-handoff.md`). It worked cross-host from the very first packet,
and the wire now carries **only RTP-framed datagrams**: no `R3P1` setup
messages. In adjacent, matched windows its throughput, loss, RTT and CPU were
**indistinguishable from AEAD within run-to-run variation**. It costs 8 more bytes
per packet. These results do **not** establish censorship resistance, valid
media camouflage, or a speedup. random-nonce has no authenticity, replay protection or
forward secrecy by design.

Source: `experiments/rtp-l3` (`client-x` / `server-x`). Endpoint-free
aggregate with per-case epochs, RTT, CPU, counters, capture shape and framing
checks: [`2026-10-24-v2-summary.json`](../experiments/rtp-l3/results/2026-10-24-v2-summary.json).
Directions are from the client's perspective. Raw evidence stays private
outside the checkout; keys and remote staging were deleted.

## Correctness

- 13 unit tests pass on macOS and Linux (6 random-nonce tests). They include a packet vector
  generated independently with libsodium `crypto_stream_xchacha20_xor`
  (counter 0) and the draft-arciszewski-xchacha-03 A.3.2 keystream.
- Disposable-namespace integration: first ping with no setup; 100 MiB each way
  across RTP sequence rollover; both peers restarted with fresh
  SSRC/sequence/nonces; loss/duplicate/reorder/corruption injection. Every
  corrupted datagram was forwarded (`rejected` stayed 0), as specified for an
  untagged cipher. Endpoint checksums discarded them. The AEAD integration still
  passes.
- Cross-host: the first ping without setup returned 1/1. 1200-byte MTU pings returned
  10/10 and pings through server forwarding/NAT to a controlled egress
  endpoint returned 10/10. The endpoint logged only the masqueraded source.

## Wire evidence

Server captures with an 80-byte snaplen had zero kernel drops.

| Capture | Packets | Non-RTP | `R3P1` control | Max IPv4 | Dominant data / ACK IPv4 sizes |
| --- | ---: | ---: | ---: | ---: | --- |
| X4 client restart | 23 | 0 | 0 | 1264 | Client's first datagram: 148 B (84 B ICMP + 64) |
| random-nonce run A, 09:56:27–09:57:45 | 200,000 | 0 | 0 | 1264 | 1264 (136,963) / 116 (58,129) |
| AEAD, 10:03:40–10:05:05 | 200,000 | 4 | 4 (one per type) | 1256 | 1256 (136,338) / 108 (55,832) |
| random-nonce run B, 10:11:01–10:12:26 | 200,000 | 0 | 0 | 1264 | 1264 (137,460) / 116 (56,494) |

Each count-limited capture covered startup, idle ping and the first
single-stream upload cases. AEAD's capture opens with its 97/129-byte control
exchange. In random-nonce, the first packet in each direction is ordinary RTP-framed
ciphertext. One new observable: `server-x` forwards whatever its new TUN emits to
the configured peer. Small server-originated datagrams therefore appeared
before the client's first packet (inner 40 and 51 bytes; not decrypted). AEAD sends nothing
before a handshake. Packet-size modes, sub-millisecond bursts (up to ~750 packets
at ≤1 ms gaps) and 90 kHz timestamps look the same as AEAD, except for the +8
bytes. Neither configuration resembles codec media.

Overhead: 64 vs 56 IPv4 bytes per packet, so 5.06% vs 4.46% of a full
1264/1256-byte packet. A bare 52-byte TCP ACK grows from 108 to 116 bytes (+7.4%).

## TCP goodput and tunnel CPU (adjacent A-B-A windows)

Each cell is the median of three 20-second runs, with the range below it. CPU is
the median percentage of **one core** used by the tunnel process, shown as
client / server.

| Case | random-nonce run A | AEAD | random-nonce run B | CPU random-nonce A · AEAD · random-nonce B |
| --- | ---: | ---: | ---: | --- |
| Upload ×1 | 20.83 (17.20–24.35) | 18.50 (16.94–20.52) | 18.91 (17.88–19.43) | 10.3/6.4 · 9.8/6.5 · 8.6/5.6 |
| Upload ×4 | 40.20 (37.93–42.38) | 43.31 (35.90–44.51) | 39.12 (35.44–42.80) | 19.1/11.8 · 22.7/14.9 · 17.7/12.0 |
| Download ×1 | 35.91 (35.33–36.23) | 35.70 (34.18–37.96) | 35.70 (34.76–37.80) | 14.9/9.7 · 15.8/10.5 · 14.6/9.6 |
| Download ×4 | 132.32 (129.71–133.69) | 133.42 (133.06–140.40) | 137.46 (134.84–142.13) | 28.4/33.3 · 32.7/36.5 · 28.4/34.2 |

Rates are in Mbit/s. The ranges overlap in every case, so there is no throughput
difference to report. Four-stream CPU was a few points lower in both random-nonce runs.
This was despite a `getrandom` call and an HChaCha20 subkey derivation for every
packet. The effect is small, comes from one window, and is not established. Peak
RSS was about 2.4–2.6 MB per side.

## Paced UDP (1000-byte payload, 10 s per rate, stop above 2% loss)

| Direction | random-nonce run A | AEAD | random-nonce run B |
| --- | --- | --- | --- |
| Upload | 5M 0%; **10M 4.23% stop** | 5M 0%; 10M 0%; **20M 2.17% stop** | 5M–50M ≤0.004%; **100M 49.16% stop** (46.6 delivered) |
| Download | 5M–100M all 0.99–1.48% | 5M–100M all 0.91–1.37% | 5M–100M all 0.80–1.23% |

The upload ceiling varied between runs of the same configuration, from 10M to 50M+.
The ladder stopping point is noise on this path, not a protocol difference. Every
mode showed a ~1% download loss floor starting at 5 Mbit/s and stayed flat up to
100 Mbit/s. That points to path behaviour, not tunnel load. Overload at 100M upload repeats
AEAD's earlier result (only 52/121 loaded pings returned).

## RTT, counters and buffers

Idle RTT median/p99 was 236/239 ms (random-nonce A), 236/240 ms (AEAD) and 236/237 ms (random-nonce B).
Loaded TCP medians were 235–236 ms in every mode, with p99 at most 247–248 ms.
This window's path RTT was about 235 ms, versus about 178 ms in AEAD's earlier
window, so compare only within this report. Ping "sent − received" includes
probes still in flight when ping stops.

| Final counters | random-nonce A | AEAD | random-nonce B |
| --- | ---: | ---: | ---: |
| Client socket receive overflow | 11,263 | 8,288 | 12,494 |
| Server socket receive overflow | 2,948 | 3,410 | 2,861 |
| Server socket send drops | 1,146 | 1,168 | 2,165 |
| TUN write drops / rejected-or-invalid | 0 / 0 | 0 / 0 | 0 / 0 |

Both sides requested 4 MiB socket buffers and got 425,984 bytes under the
unchanged host limits. Receive overflow is still the main local loss source,
as in AEAD. Buffer tuning remains a separate, untested change.

## Operational findings

- Client-side NAT rewrites the source port. Without a handshake, the server's
  return tuple must be learned out of band. The operator procedure is in the
  handoff. In practice, the mapping stayed stable across restarts that were
  minutes apart.
- On one fixed client port range, no return UDP arrived at all, even for a plain
  UDP echo. Other fixed ports worked. The cause was not established, and
  this is not a random-nonce issue. A port was chosen by a plain echo test before use.
- Debian's tcpdump AppArmor profile blocked `-w` into the root-only task
  directory, so the first X4 capture was lost. Captures were then written through stdout.

## Windows, method and limitations

All times are UTC on 2026-10-24: random-nonce A 09:56:28–10:02:43, AEAD 10:03:40–10:10:08,
Random-nonce B 10:11:02–10:17:55. Runs were sequential with the same `benchmark.py`
settings, binary build, port tuple, buffers and host congestion control (bbr).
Nothing else ran on the hosts during the runs: no compilation and no
evidence transfer. There were no failed or retried cases. A
direct (no tunnel) baseline was not repeated. One short window on one path
cannot show peak-hour behaviour or how middleboxes classify the traffic.

## Cleanup

All task-owned firewalld **runtime** rules were removed: the client-/32 UDP
opening, the trusted-zone TUN/veth assignments and the scoped masquerade rule.
Task processes, the TUN, the veth and the egress namespace are gone. After
cleanup, the zone/policy listings, IP forwarding and link list matched the
pre-task snapshots exactly. Permanent firewalld configuration was never
touched, and the host's own pre-existing iperf3 service was left running.
Remote and client staging, binaries and all PSK copies were deleted. Only
key-free raw evidence is retained privately.
