# PR 135 continuation handoff

The user authorized Phases 1–4 on the existing PR with incremental commits and
pushes. All work is on `simophin/cpxy-ng:smoltcp-netstack-plan` / PR 135.
The current continuation branch is `pr135-continue` in worktree `87f3`;
the older `smoltcp-netstack-plan` local branch is in worktree `bbeb` and can
lag the remote. Fetch the remote PR branch before resuming.

## State

Phases 0–4 are implemented, including the approved 32-range sender SACK.
The engine pins fork `40ba802` (`simophin/smoltcp:cpxy`, pushed). This corrects
`478752b`'s inherited NewReno three-MSS inflation on SACK recovery entry:
Reno/CUBIC now use cwnd = ssthresh with SetPipe, preserving NewReno for peers
without SACK. Two regression checks fail with the old entry hook. The plan
records the original matrix separately from corrected-pin measurements.

**The performance gate remains unmet; do not recommend rollout.** The latest
full 200 MB download at 0.5% loss reaches 1.8 MB/s (86.2 CPU-ms/MB), versus
ipstack's recorded 2.3 MB/s and the previous `d64e86d` sample's 0.9 MB/s.
Single random-loss gateway samples do not establish an exact speedup.

The user requested a focused investigation and an early flag if more work is
not easy. One small CUBIC bug is fixed: `40ba802` counts all newly cumulatively
ACKed bytes in the Reno-friendly estimate rather than at most one MSS.
Six seeded simulations improve by 15–37%. All 743 fork tests, three netsim
cases, no-std check and 61 Linux engine tests pass. CI is green on `adc1a4e`
(run `39993119344`, including TUN and both Docker architectures).

**Decision checkpoint:** no further substantial TCP work was started. The
completed packet traces show that disabling loopback offloads eliminates the
observed bulk RTOs in one 20 MB/1% sample, yet throughput remains near 0.8 MB/s.
This does not justify assuming a RACK/TLP implementation will close the gap.
The user has been told the remaining uplift is not demonstrated to have another
small, safe fix. Keep ipstack in production / this PR experimental until the
next route or acceptance tradeoff is chosen. Do not waive the gate or alter
congestion policy. Phase 5 remains outside the requested scope.

The plan's [focused investigation](smoltcp-netstack-plan.md#focused-performance-investigation-2026-10-24)
contains exact samples, the loss-model caveat and alternatives. For a BBR
kernel reference, another prepared host is needed: `cn-baise` only exposes
Reno and CUBIC, and the user prohibits host root operations.

## Investigation completed

- Current fork fix: `40ba802`, ACK-accounting regression fails before the fix;
  seeded before/after logs are `/private/tmp/pr135-cubic-ack-{baseline,fixed}.log`
  locally. Packet-header tracing is opt-in with
  `RUST_LOG=warn,cpxy_tsgw=info,mobile_engine::netstack::wire=trace,smoltcp::socket::tcp=trace`.
  Keep the gateway info target or the harness cannot see readiness/approval.
- Current traces: `measurements/cubic-ack-1pct-wire-final.log` (default offloads)
  and `cubic-ack-1pct-wire-split.log` (offloads disabled), with corresponding
  `trace-final`/`trace-split` transfer logs. Each moves 20 MB at 1% loss in about
  24 seconds of bulk transfer. There are eight bulk RTOs with default offloads,
  zero with offloads disabled, and two additional default-mode warm-up RTOs
  that must not be confused with the bulk flow. Missing-frontier bytes had not
  previously been retransmitted. The RTO floor is 200 ms; RTT samples 55–62 ms.
- `NETEM_OFFLOADS=off` on `bench/delayed.sh` disables loopback TSO/GSO/GRO/UDP
  segmentation inside the namespace and requires ethtool. Default `preserve`
  matches historical results. Netem makes its loss decision per skb, so UDP
  GSO batching can change the loss model. Record/match modes before comparing.
  The packet experiment is diagnostic, not a replacement acceptance matrix.
- Two earlier trace setup failures are preserved, not performance results.
  The first filter hid the readiness log; the second failed to observe route
  approval. The final bounded attempt succeeded. There is no open trace task.

- Added `tsgw/test/bench/kernel.sh`, a kernel CUBIC reference with MTU 1280,
  TSO/GSO/GRO disabled, netem on loopback and iperf3 JSON. It needs ethtool
  (extracted without installation into `measurements/tools/usr/sbin`). The
  256 KiB request gives actual Linux buffers of 425,984 bytes. At 54 ms RTT,
  forward/reverse 30-second runs yield 3.80–3.82 MB/s at 0%, 0.72–0.76 MB/s
  at 0.5%, and 0.37–0.38 MB/s at 1% loss. A 1 MiB request is unsupported by
  the host's buffer limits; changing them inside a user namespace is denied.
  Host settings and existing services were left unchanged. These references
  show kernel CUBIC's loss penalty but are not an exact gateway comparison or
  proof of a ceiling for every compliant stack.
- Enabled the fork's `log` feature. Four short 20 MB debug runs on `478752b`
  all finish, including uploads, with no `assembler: too many holes` messages.
  Do not increase the receiver's four-range capacity without evidence. The
  upload sender is Tailscale's gVisor CUBIC stack, not Linux TCP; the earlier
  handoff's attribution was wrong.
- At 1% loss the 20 MB download logs 13 retransmission timeouts (including
  warm-up), versus 2 at 0.5%. The deficit is not established to be solely
  CUBIC's response to loss. Further cause analysis needs packet traces that
  distinguish lost tails, repairs and ACKs. No RACK/TLP implementation was added.
- `TestStateFromGo` passes in its own unprivileged network namespace. The
  earlier native address conflict is environmental; do not stop host services.
- The original sender-SACK PR CI passed all checks (run `39970709809`). The
  final continuation's CI status and native verification are recorded in the plan.

## Code and repositories

- `mobile-engine/src/netstack/`: feature-gated async TCP/UDP adapter, ten packet
  tests plus one ignored idle-socket profiler. tsgw enables `mobile-engine/smoltcp`;
  mobile/router defaults retain ipstack; packaging builds gateway and router
  separately to avoid feature unification.
- Fork `simophin/smoltcp:cpxy` at `40ba802`: NewReno (`da00976`), #1221 ACK
  sequencing (`8774bfc`), upstream receiver SACK from #1195 (original authorship
  retained), the SACK entry correction (`d64e86d`), CUBIC ACK accounting
  (`40ba802`), and the sender SACK
  scoreboard (`src/socket/tcp/sack.rs`, changes in
  `src/socket/tcp.rs`, tests in both, `tests/netsim.rs` runs the CUBIC loss case
  with and without SACK and has an ignored env-driven exploration test).
  Local checkout `/private/tmp/cpxy-smoltcp`, clean and pushed; `target/ci-*`
  subdirectories there are disposable build caches from the CI feature sets.
  Review branches `cpxy-newreno` and `cpxy-rto-ack` are unchanged.
- Upstream's contribution policy requires human-authored submissions; nothing
  generated was submitted upstream.
- GitHub identity for writes: `GH_CONFIG_DIR=$HOME/.config/gh/personal`, login
  `simophin`, push over HTTPS with `-c credential.helper='!gh auth
  git-credential'` and clear other helpers with `-c credential.helper=`.
  SSH authenticates as the work account; do not use it for GitHub pushes.

## Validation at handoff

- Fork: 258 TCP-module tests (5 scoreboard, 10 sender-SACK) and 742 tests in the
  `default,socket-tcp-reno,socket-tcp-cubic` set (`--lib --tests`; examples do
  not build on macOS), IPv6-only set, no-std check, all three netsim tests with
  updated snapshots, `cargo fmt`; clippy has one pre-existing warning.
- Previous corrected pin: all 742 fork unit tests and three netsim cases, no-std check
  and formatting pass; native Linux passes 61 feature-enabled tests and the
  real-TUN lab. Default-engine coverage also passes in CI. Isolated state import
  passes. The first no-delay upload stalls and hits Go's hard deadline; the
  short-deadline retry passes at 74.7 MB/s. Both logs are preserved.
- Benchmark matrix for `478752b` complete (12 runs, logs `measurements/sack-*.log`,
  summary `measurements/sack-matrix.log`). Corrected-pin gate logs are
  `sack-window-262144-*`; the 0.5% gate fails, so larger-buffer/loss cases were
  not expanded. No task-owned build, benchmark or gateway process remains running.
- All applicable checks pass on current CUBIC-fix code commit `adc1a4e`, CI
  run `39993119344`, including both Docker architectures. The later offload-mode
  script and documentation update have their own subsequent CI run.

## Linux workspace and tools

Use `ssh cn-baise` as the regular user; no root. The login shell is fish, wrap
commands in `sh -c`. A separate systemd/podman cpxy gateway container runs on
the host (`cpxy-tsgw`, `cpxy-client` under `conmon`); it is not this task's and
likely explains the earlier `Address already in use` in `TestStateFromGo`.

Workspace `$HOME/Temp/cpxy-pr135` is an rsync copy of the continuation branch
(not a git repository; sync with `rsync -az --delete --exclude target
--exclude .git --exclude node_modules --exclude measurements --exclude .claude`).
Use `RUSTUP_TOOLCHAIN=1.97.1`, add `$HOME/.cargo/bin`, `$HOME/Temp/tsgw-dev/go/bin`
and `/usr/sbin` to `PATH`, unset the proxy variables, `GOPROXY=https://goproxy.cn,direct`.
Disk is at roughly 2 GiB free; do not add debug builds. Release builds used
`CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=false`.

`measurements/` binaries: `cpxy-tsgw-ipstack`, `-phase2` (`24bda3b`), `-phase3`
(`8774bfc`), `-phase4` (`01bf32a`), `-sack` (`478752b`), `-logged`
(`478752b` with logging), `-sack-window` (`d64e86d`), and `-cubic-ack`
(`40ba802` plus packet-header diagnostics). `bins/` holds the lab
`cpxy-server` and feature-enabled `mobile-engine-linux`. Helpers in `$HOME/Temp/`:
`pr135-measure.sh <label> <binary> "<windows>" ["<losses>"] ["<dirs>"]` skips
existing logs; `pr135-perf.sh` captures a profile. Do not compile while
measuring. Uploads use a 120 s limit and downloads 300 s; a timeout is a
censored measurement, and the logs state the bytes transferred.

Direct dependency fetching stalled during the corrected build. The exact pushed
fork is preserved as `measurements/pr135-smoltcp.bundle` and was imported into
`$HOME/.cargo/git/db/smoltcp-060853523aea0f3b`; subsequent builds work with
`CARGO_NET_OFFLINE=true`. No dependency revision was substituted.

Additional raw results: `kernel-*.json` / `kernel-*.log`, `debug4-*`,
`state-isolated.log`, `sack-window-*.log`. Short debug samples are diagnostics,
not replacements for full-size throughput results. Kernel and gateway
measurements must run serially, without simultaneous compilation on the host.

The current fork bundle is `measurements/pr135-cubic-ack.bundle`, imported into
the same Cargo git database. Current gate log: `cubic-ack-262144-0.5%-D.log`.
Local packet analysis script: `/private/tmp/pr135-analyze-wire.py`; summaries
are `/private/tmp/pr135-cubic-wire-{summary,split-summary}.json`. For RTO
attribution, verify the next emitted packet's ports/sequence: a generic RTO
log line alone does not identify which connection timed out.
