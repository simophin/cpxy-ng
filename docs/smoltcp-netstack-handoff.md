# PR 135 continuation handoff

The user asked for Phases 1–4 on the existing PR with incremental commits and
pushes, then on 2026-10-24 asked to wind down and hand off. All changes are on
`simophin/cpxy-ng:smoltcp-netstack-plan` / PR 135; the PR description has not
been updated for the sender-SACK work yet.

## State

Phases 0–4 are implemented and tested. The approved bounded sender-side SACK is
in fork commit `478752b` (`simophin/smoltcp`, branch `cpxy`, pushed) and the
engine pins it (`ca00224` on the PR branch). Details, the full measurement table
and the simulation analysis are in [the plan](smoltcp-netstack-plan.md).

**The loss-performance gate is still unmet; do not claim the migration is
ready for rollout.** Sender SACK made the 0.5% downloads complete (1.2 MB/s at
256 KiB, 1.5 MB/s at 1 MiB; earlier builds were censored at ~0.6 MB/s) but
ipstack's uncontrolled window still does 2.3 MB/s at 256 KiB, the 1% downloads
deliver ~0.47 MB/s and miss the 300 s deadline, and every lossy upload (kernel
sender, smoltcp receiver) moves 0.33–0.47 MB/s and misses its 120 s deadline.
Keep the gateway's 256 KiB default.

What the simulation showed: RFC 6675 recovery repairs every hole in a window
within one round trip and does not add timeouts, but it is 0–20% slower than
NewReno under random loss in the fork's netsim because NewReno's duplicate-ACK
inflation (forbidden by RFC 6675) keeps its pipe fuller during its longer
episodes and leaks into the next episode's ssthresh. The spec behaviour was kept
on purpose. The gateway's problem is CUBIC's response to random loss, not loss
recovery.

## Open questions and suggested next steps

1. **Kernel TCP reference (not run).** To show what any compliant stack can do
   on this fixture, run iperf3 (installed on `cn-baise`) in its own namespace
   with netem on all of `lo`, socket buffers capped like the gateway's:
   `unshare --map-root-user --net sh -euc 'ip link set lo up; tc qdisc add dev
   lo root netem delay 27ms loss 0.5% limit 100000; iperf3 -s -D -1; sleep 0.5;
   iperf3 -c 127.0.0.1 -w 256K -t 60 -f M'`, also with `-R`, `-w 1M`, and 0% and
   1% loss. If kernel-to-kernel is near 1.2–1.5 MB/s at 0.5%, the download side
   is at its ceiling and the gate (parity with uncontrolled ipstack) cannot be
   met by congestion-controlled TCP; the user then decides whether the gate or
   the congestion response changes. Do not run it while another measurement is
   running.
2. **Uploads (smoltcp as receiver).** Unmeasured suspects: the reassembler holds
   `ASSEMBLER_MAX_SEGMENT_COUNT = 4` contiguous ranges (three holes); with a
   kernel sender pushing ~180 segments per window at 1% loss, a fourth hole makes
   smoltcp drop the segment (`assembler: too many holes`). smoltcp exposes
   `assembler-max-segment-count-32` as a cargo feature; adding it to the
   `smoltcp` dependency features in `mobile-engine/Cargo.toml` is a one-line
   experiment, untested. Also compare `set_ack_delay(None)` as the plan suggests.
   Check the gateway log in the failed upload runs for the assembler message
   before changing anything.
3. **1% downloads.** ~0.47 MB/s is below the 1/√2 scaling from 0.5%, which
   suggests timeouts; count `retransmitting after rto` with the engine's log at
   debug level before tuning anything.
4. The remaining RFC 6675 inefficiency, a rescue or a new episode's first
   retransmission resending a repair still in flight (12–16 segments per 10 MB
   at 1% in netsim), costs under 1% and was left alone.
5. Update the PR description (title and body still describe receiver SACK as
   the latest work) and let CI run on `ca00224`; it has not been checked.

## Code and repositories

- `mobile-engine/src/netstack/`: feature-gated async TCP/UDP adapter, ten packet
  tests plus one ignored idle-socket profiler. tsgw enables `mobile-engine/smoltcp`;
  mobile/router defaults retain ipstack; packaging builds gateway and router
  separately to avoid feature unification.
- Fork `simophin/smoltcp:cpxy` at `478752b`: NewReno (`da00976`), #1221 ACK
  sequencing (`8774bfc`), upstream receiver SACK from #1195 (original authorship
  retained), and the sender SACK scoreboard (`src/socket/tcp/sack.rs`, changes in
  `src/socket/tcp.rs`, tests in both, `tests/netsim.rs` runs the CUBIC loss case
  with and without SACK and has an ignored env-driven exploration test).
  Local checkout `/private/tmp/cpxy-smoltcp`, clean and pushed; `target/ci-*`
  subdirectories there are disposable build caches from the CI feature sets.
  Review branches `cpxy-newreno` and `cpxy-rto-ack` are unchanged.
- Upstream's contribution policy requires human-authored submissions; nothing
  generated was submitted upstream.
- GitHub identity for writes: `GH_CONFIG_DIR=$HOME/.config/gh/personal`, login
  `simophin`, push over HTTPS with `-c credential.helper='!gh auth
  git-credential'` (see the memory note; SSH authenticates as the work account).

## Validation at handoff

- Fork: 258 TCP-module tests (5 scoreboard, 10 sender-SACK) and 742 tests in the
  `default,socket-tcp-reno,socket-tcp-cubic` set (`--lib --tests`; examples do
  not build on macOS), IPv6-only set, no-std check, all three netsim tests with
  updated snapshots, `cargo fmt`; clippy has one pre-existing warning.
- Engine: ten netstack tests on macOS; native Linux 61 (feature) and 51
  (default) library tests; real-TUN lab passes with the feature.
- Benchmark matrix for `478752b` complete (12 runs, logs `measurements/sack-*.log`,
  summary `measurements/sack-matrix.log`); no measurement process left running.
- CI for `ca00224` was not checked.

## Linux workspace and tools

Use `ssh cn-baise` as the regular user; no root. The login shell is fish, wrap
commands in `sh -c`. A separate systemd/podman cpxy gateway container runs on
the host (`cpxy-tsgw`, `cpxy-client` under `conmon`); it is not this task's and
likely explains the earlier `Address already in use` in `TestStateFromGo`.

Workspace `$HOME/Temp/cpxy-pr135` is an rsync copy of the PR branch at
`ca00224` (not a git repository; sync with `rsync -az --delete --exclude target
--exclude .git --exclude node_modules --exclude measurements --exclude .claude`).
Use `RUSTUP_TOOLCHAIN=1.97.1`, add `$HOME/.cargo/bin`, `$HOME/Temp/tsgw-dev/go/bin`
and `/usr/sbin` to `PATH`, unset the proxy variables, `GOPROXY=https://goproxy.cn,direct`.
Disk is at ~3.4 GiB free; do not add debug builds. Release builds used
`CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false`.

`measurements/` binaries: `cpxy-tsgw-ipstack`, `-phase2` (`24bda3b`), `-phase3`
(`8774bfc`), `-phase4` (`01bf32a`), `-sack` (`478752b`). `bins/` holds the lab
`cpxy-server` and feature-enabled `mobile-engine-linux`. Helpers in `$HOME/Temp/`:
`pr135-measure.sh <label> <binary> "<windows>" ["<losses>"] ["<dirs>"]` skips
existing logs; `pr135-perf.sh` captures a profile. Do not compile while
measuring. Uploads use a 120 s limit and downloads 300 s; a timeout is a
censored measurement, and the logs state the bytes transferred.
