# Historical header-derived nonce data-format results

Historical measurement record. The current design is specified in
[the protocol specification](rtp-l3-protocol.md). Artifact filenames and recorded
protocol identifiers are frozen evidence, not supported runtime modes. These
measurements predate the unified key labels/reserved-zero control header and
do not validate those new wire conventions.

This data-format experiment used handshake-free raw
XChaCha20 with a shorter nonce field on the wire. The 24-byte nonce is RTP
header bytes 2..10 (sequence, timestamp, high SSRC half) followed by **16
random bytes** carried in the packet. Per-packet overhead drops back to AEAD's
56 IPv4 bytes. The current full protocol is in `docs/rtp-l3-protocol.md`; this record covers
the earlier data-only experiment.

Header-nonce delivered the first packet with no setup, and captures contained only
RTP-framed datagrams. Data packets now have exactly AEAD's sizes, so the only
visible wire difference from AEAD is the missing `R3P1` handshake. In adjacent
Header-nonce / AEAD / header-nonce windows, throughput, loss, RTT, drops and CPU overlapped AEAD. The
path drifted during this window, which makes the comparison weaker than random-nonce's
(see below). header-nonce then replaced AEAD in the code. These results do **not**
establish censorship resistance, valid media camouflage or a speedup. header-nonce has
no authenticity, replay protection or forward secrecy by design.

Aggregate (endpoint-free; per-case epochs, RTT, CPU, counters, capture shape,
framing): [`2026-10-24-v3-summary.json`](../experiments/rtp-l3/results/2026-10-24-v3-summary.json).
Directions are from the client's perspective. random-nonce results are in
`docs/rtp-l3-random-nonce-results.md`.

## Nonce safety

Uniqueness rests on the 128 random bits; the header prefix only adds margin.
The chance of any repeat among n packets under one key is about n²/2¹²⁹:
roughly 1.5×10⁻¹⁶ for a year of nonstop traffic at 10k packets/s, and roughly
10⁻¹⁰ at 2⁴⁸ packets. A repeat would expose the XOR of that one pair of
plaintexts, never the key. This assumes the OS random generator never repeats
its output (for example after a VM snapshot is restored).

## Correctness

- 13 unit tests passed on macOS and Linux before AEAD removal, including a new
  packet vector generated independently with libsodium
  `crypto_stream_xchacha20_xor` for the header-prefix layout. Other tests show
  that identical headers still get fresh random bytes, that header tampering
  garbles decryption, and that the low SSRC half is framing only.
- Disposable-namespace integration passed: first ping with no setup, 100 MiB
  each way across RTP rollover, both peers restarted, fault injection.
- Cross-host: first ping without setup 1/1, 1200-byte MTU pings 10/10, and
  pings through server forwarding/NAT 10/10 (masqueraded source confirmed).
  Five-second smoke TCP: up 31.42, down 24.53, forwarded 12.01 Mbit/s.

## Wire evidence

Server captures (80-byte snaplen) had zero kernel drops.

| Capture | Packets | Non-RTP | `R3P1` | Max IPv4 | Dominant data / ACK sizes |
| --- | ---: | ---: | ---: | ---: | --- |
| Validation run | 59,219 | 0 | 0 | 1256 | Client's first datagram: 140 B (84 B ICMP + 56) |
| header-nonce run A, 10:59:04–11:00:11 | 200,000 | 0 | 0 | 1256 | 1256 (137,966) / 108 (55,831) |
| AEAD, 11:06:03–11:07:05 | 200,000 | 4 | 4 | 1256 | 1256 (137,554) / 108 (57,854) |
| header-nonce run B, 11:13:08–11:14:18 | 200,000 | 0 | 0 | 1256 | 1256 (138,779) / 108 (58,074) |

As in random-nonce, the server forwards packets that its new TUN emits to the
configured peer before the client sends anything.

## TCP goodput and tunnel CPU (adjacent A-B-A windows)

Each cell is the median of three 20-second runs (range in brackets), in Mbit/s.
CPU is the median percentage of **one core** used by the tunnel process, shown
as client / server.

| Case | header-nonce run A | AEAD | header-nonce run B | CPU header-nonce A · AEAD · header-nonce B |
| --- | ---: | ---: | ---: | --- |
| Upload ×1 | 25.44 (23.73–31.71) | 25.18 (18.70–38.39) | 22.54 (15.75–34.25) | 12.6/7.7 · 14.9/9.4 · 11.4/7.1 |
| Upload ×4 | 43.88 (41.34–43.88) | 40.00 (37.46–42.38) | 43.83 (43.47–44.24) | 20.9/13.2 · 21.4/13.2 · 19.7/12.7 |
| Download ×1 | 31.87 (20.34–34.13) | 29.41 (28.36–30.30) | 27.10 (26.48–28.68) | 14.7/8.6 · 15.2/9.5 · 13.1/8.4 |
| Download ×4 | 121.58 (111.14–125.30) | 105.32 (103.80–108.05) | 106.69 (84.25–108.15) | 28.9/31.0 · 30.5/32.3 · 26.9/26.8 |

**The path drifted during this window.** Download goodput fell steadily from header-nonce
run A to AEAD to header-nonce run B; the earlier random-nonce window had about 132 Mbit/s with four
download streams. The two runs closest in time to each other, AEAD and header-nonce run B,
match within their ranges (105.3 vs 106.7). Upload ranges overlap in every case.
There is no throughput difference to report. Tunnel CPU was the same or slightly
lower for header-nonce in every case. That is consistent with the random-nonce window, but the effect
is small and not established. Peak RSS was about 2.3–2.6 MB per side.

## Paced UDP (1000-byte payload, 10 s per rate, stop above 2% loss)

| Direction | header-nonce run A | AEAD | header-nonce run B |
| --- | --- | --- | --- |
| Upload | 5M–50M 0%; **100M 48.80% stop** (50.0 delivered) | 5M–50M 0%; **100M 48.74% stop** (50.1 delivered) | 5M, 10M 0%; **20M 5.79% stop** |
| Download | **5M 2.59% stop** | **5M 2.31% stop** | **5M 4.09% stop** |

In this window, download loss at 5 Mbit/s was 2–4% for every protocol (the
earlier window had about 1%), so every download ladder stopped at its first
step. Upload capacity was about 50 Mbit/s for both header-nonce run A and AEAD. header-nonce run B's
stop at 20M matches the same run-to-run spread already seen in the random-nonce experiment (10M to 50M+).

## RTT, counters and buffers

Idle RTT median/p99 was 237/237 ms for all three runs. Loaded TCP medians were
236–237 ms, with p99 at most 243 / 249 / 244 ms. Ping "sent − received"
includes probes still in flight when ping stops.

| Final counters | header-nonce A | AEAD | header-nonce B |
| --- | ---: | ---: | ---: |
| Client socket receive overflow | 3,483 | 1,437 | 1,500 |
| Server socket receive overflow | 4,408 | 5,888 | 3,777 |
| Server socket send drops | 5 | 278 | 507 |
| TUN write drops / rejected-or-invalid | 0 / 0 | 0 / 0 | 0 / 0 |

Socket buffers were still 425,984 bytes (host limits unchanged).

## Windows, method and limitations

All times are UTC on 2026-10-24: header-nonce A 10:59:05–11:05:09, AEAD 11:06:03–11:12:06,
Header-nonce B 11:13:09–11:18:50. Settings were identical to the random-nonce comparison: same
binary for both protocols, same tuple, same buffers, bbr. Nothing else ran on
the hosts during the runs, and no case failed. The client NAT mapping kept the
same external port across all runs. No direct (no tunnel) baseline was run.
Path drift during the window limits any conclusion beyond "no measurable
difference".

## Cleanup

All runtime firewalld rules, interface assignments, processes, TUN/veth and
the egress namespace were removed. The zone/policy listings, IP forwarding
and link list exactly match the snapshots taken before this deployment.
Permanent firewalld config and the host's own iperf3 service were untouched.
Server and client staging, binaries and all PSK copies were deleted. Only
key-free raw evidence is retained privately.
