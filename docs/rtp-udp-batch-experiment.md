# UDP batch adapter experiment — 2026-10-28

The first adapter prototype passes protocol and platform checks, but **router
loopback results do not justify enabling it in production**. This is an early
step 2 experiment in the [packet-path plan](rtp-packet-path-efficiency-plan.md),
not completion of the profiling or transport acceptance gates.

## What is implemented

`rtp-l3::v2::udp_io` provides explicitly selected portable and Linux mmsg
backends, bounded to 1–64 datagrams per call. Linux uses nonblocking
`recvmmsg`/`sendmmsg`. Receive packet storage, source addresses, ancillary
buffers, iovecs and message descriptors are allocated once and reused. Neither
backend waits for a full batch or starts a thread. No new dependency, operator
setting, MTU or wire format change is introduced.

Every receive exposes its own source tuple and truncation flag. Oversized frames
cannot be obtained through `frame()`. Linux optionally decodes cumulative
`SO_RXQ_OVFL` ancillary counts; these are socket-scoped, may wrap and must reset
with socket identity. Portable receives use an extra byte to reject oversized
protocol frames. They do not expose ancillary counts.

Sends borrow already-encrypted frames and return only a successfully sent
prefix. The caller retains ownership of the suffix and must retry identical
ciphertext. The adapter does not reserve packet numbers or touch authentication,
replay, path, routing or allocation state. No worker uses it yet. Consequently,
production queue retention, lifecycle invalidation and readiness integration are
still required; these are not established by the adapter's unit tests.

## Validation

- All 25 `rtp-l3` library tests pass with glibc and static x86-64 musl, including
  the existing independent crypto vectors, tamper, replay and nonce checks.
- Five new adapter tests cover IPv4/IPv6 source tuples and datagram boundaries,
  full/oversized/zero-length receives, descriptor reuse after truncation,
  16/32/64 budgets, invalid budgets, scoped IPv6 and ancillary layouts,
  authenticated/tampered/replayed frames, and a real partial-send result.
  The partial-send test forces an error at the second destination, then retries
  the original encrypted suffix and verifies both ciphertexts and replay state.
  It does not emulate production EAGAIN queue handling.
- x86-64 musl, ARM64 musl and iOS library compile checks pass. The ARM64 static
  benchmark ran on the physical OpenWrt router, exercising the musl ABI there.
- Rustfmt and diff checks pass. Clippy passes on the available stable toolchain
  with only the existing `deprecated` and `collapsible_if` warnings allowed
  (`http_io.rs` and `client_runtime.rs`). The pinned 1.97.1 toolchain has no
  Clippy component installed.

Cross-compilation found musl's private `msghdr` padding and unsigned mmsg flags;
headers now initialize the entire ABI structure before assigning public fields,
and flag arguments use the target libc signature.

## Bounded loopback measurements

The example sends one batch, receives and verifies every byte and source tuple,
then repeats on one thread. It uses 1,232-byte synthetic frames, with no crypto,
TUN, DNS, policy, socketpair shim, NAT or WAN. CPU is process user plus system
CPU from `getrusage`; it includes both send and receive on the same machine.
Kernel softirq/helper CPU outside the process is not included. These are adapter
microbenchmarks, not total router CPU per delivered tunnel byte.

Each budget used three portable/mmsg/portable sequences, giving six portable
and three mmsg samples. Each development-host window contained 10,000 batches;
each router window contained 1,000 batches, with a one-second pause between
windows. Every window verified all expected datagrams and bytes; none were
excluded. No CPU/IRQ/offload settings were changed. The router service kept
running; the temporary benchmark binary removed itself on exit.

| Budget | Development portable CPU ns/datagram (range) | Development mmsg (range) | Router portable (median) | Router mmsg (median) |
| --- | ---: | ---: | ---: | ---: |
| 16 | 8,333–9,270 | 6,257–7,274 | 12,856 | 13,290 |
| 32 | 8,314–9,635 | 6,324–7,228 | 13,083 | 13,652 |
| 64 | 8,385–9,732 | 6,251–6,390 | 14,355 | 14,983 |

Raw results: [development host](../experiments/rtp-l3/results/2026-10-28-udp-batch-loopback.json)
and [router](../experiments/rtp-l3/results/2026-10-28-udp-batch-router-loopback.json).
Adapter call counts in these files are not measured syscall counts: the portable
backend performs a syscall for each message, while mmsg submits the vector.
No syscall profiler was installed on the router.

Router median process CPU per datagram increased approximately 3–4%. Loopback
combines local transmit and receive kernel work and uses synthetic bursts; this
cannot predict separate endpoint/router performance. The development-host gain
therefore supplies no hardware acceptance claim. The router result also gives
no evidence for moving to io_uring.

## Reproduction and next gate

```sh
cargo test -p rtp-l3 --lib --locked
cargo build -p rtp-l3 --release --example udp-batch-bench --locked
# Repeat this sequence three times for each budget 16, 32 and 64:
target/release/examples/udp-batch-bench portable 32 10000
target/release/examples/udp-batch-bench mmsg 32 10000
target/release/examples/udp-batch-bench portable 32 10000
```

The prototype remains an explicitly invoked experiment. Finish allocation/queue
instrumentation and the loaded reference profile before selecting a production
backend. A tunnel experiment must retain unsent ciphertext in bounded
allocation-owned queues, invalidate queued work on path/generation changes,
poll appropriate readiness without starving control/DNS, and handle unavailable
Linux syscalls with a portable fallback. Compare separate sender/receiver hosts
and the real packet path with adjacent reference/change/reference windows,
including latency and system-wide CPU. Only those results can satisfy the plan's
15% gate. No io_uring, socketpair removal or worker sharding has started.
