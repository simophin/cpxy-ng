# PR 135 continuation handoff

The user requested Phases 1–4 on the existing PR, with incremental commits and
pushes. They have now asked to tie up and hand off to another agent. All changes
are on `simophin/cpxy-ng:smoltcp-netstack-plan` / PR 135.

## Next work and authorization

**The user explicitly approved the bounded sender-SACK implementation** on
2026-10-23, choosing “Approve the bounded sender-SACK implementation.” They then
requested this handoff before implementation began. Do not ask for approval again.
Implement the [agreed 32-range RFC 6675 proposal](smoltcp-netstack-plan.md#bounded-sender-sack-proposal-approved-not-implemented)
in `simophin/smoltcp:cpxy`, with regressions, then pin its revision here and measure.
No sender scoreboard code has been written yet.

The current implementation fails the loss-performance gate: Phase 3 receives
186.4/200 MB in 300 seconds at 0.5% loss and 54 ms RTT (~0.62 MB/s over that
interval), versus ipstack's completed 2.3 MB/s transfer. The full table and CPU
profile are in the plan. Do not claim the migration is ready for rollout. Keep
the gateway's 256 KiB default until measurements support changing it. Remaining
loss matrices were stopped at the documented gate, rather than silently passed.
The loss-only fixture has no bandwidth bottleneck; distinguish CUBIC's response
to random loss from loss-recovery defects when interpreting results.

## Code and repositories

- `mobile-engine/src/netstack/`: feature-gated async TCP/UDP adapter. Ten packet
  tests cover IPv4/IPv6, MSS, 1 MB transfers, simultaneous half-closes, resets,
  tuple isolation, duplicate SYNs, 1,000 closes, UDP expiry and backpressure.
  One additional ignored test profiles established idle connections.
- tsgw enables `mobile-engine/smoltcp`; mobile/router defaults retain ipstack.
  Packaging builds gateway and router separately to avoid feature unification.
- The engine pins fork revision `01bf32a8934bd81e4a6f18ad40648ed940cc3147`.
  It includes NewReno (`da00976`), #1221 ACK sequencing (`8774bfc`), upstream
  receiver SACK from #1195 (original authorship retained), and reset/RTO tests.
- Local fork checkout: `/private/tmp/cpxy-smoltcp`, branch `cpxy`, clean and pushed.
  Independent review branches `cpxy-newreno` and `cpxy-rto-ack` are also pushed.
  The latter has a separate local worktree at `/private/tmp/cpxy-smoltcp-rto-upstream`.
- Upstream has no published sender-SACK branch/PR from the receiver-work author.
  Its contribution policy requires human-authored submissions, so no generated
  upstream PRs were opened. Keep the new work in the user's fork.
- GitHub identity: inspect the canonical repository `.envrc`; use
  `GH_CONFIG_DIR=$HOME/.config/gh/personal`, verify login `simophin` before writes,
  and override Git credentials with that environment's `gh auth git-credential`.
  Do not use the work account or a work-account fork.

## Validation at handoff

- Fork: 726 unit tests and all three netsim cases pass.
- Final native receiver-SACK engine: 61 passed, one manual profiler ignored;
  real-TUN lab passes. Default Linux engine previously passed 51 tests.
- `go vet ./...` and benchmark formatting pass. The no-delay 20 MB gateway
  throughput smoke test passes at ~74 MB/s in each direction.
- The following native `TestStateFromGo` fails with `Address already in use`
  when launching the gateway. No cause established; retry in an isolated user/
  network namespace. Do not change unrelated services or call this a stack bug.
- CI run `39952887957` on `4b53f15` passed all checks, including engine/TUN,
  gateway/state import, workspace tests and both Docker architectures. The final
  handoff/benchmark-diagnostic commit will trigger a new run.
- macOS's existing packet-info/TUN tests have known hangs. Use Linux for full
  engine/TUN validation; the plan records the local skips and fixture corrections.

## Linux workspace and tools

Use `ssh cn-baise` as the regular user; no host root/sudo is permitted or needed.
The login shell is fish, so wrap scripted commands in `sh -c` when appropriate.

Workspace: `$HOME/Temp/cpxy-pr135`. Its source is now the receiver-SACK revision.
Use `RUSTUP_TOOLCHAIN=1.97.1` (installed already; default Rust is older) and add
`$HOME/.cargo/bin`, `$HOME/Temp/tsgw-dev/go/bin` and `/usr/sbin` to `PATH`.
Go is 1.27.1. The host supports unprivileged user/network namespaces, TUN,
netem, nftables and own-process perf. No live Tailscale API key was used.

The login environment has stale localhost proxy settings. For Go preparation,
unset upper/lowercase `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY`, and use
`GOPROXY=https://goproxy.cn,direct` if dependencies need downloading. Fetch/
compile Go dependencies before entering a fresh network namespace (no DNS there).

`measurements/` contains raw logs, perf data/report and preserved binaries:

- `cpxy-tsgw-ipstack`: original baseline.
- `cpxy-tsgw-phase2`: unpatched fork revision `24bda3b`.
- `cpxy-tsgw-phase3`: recovery revision `8774bfc`, with profiling symbols.
- `cpxy-tsgw-phase4`: receiver SACK revision `01bf32a`.

Helpers in `$HOME/Temp/`: `pr135-measure.sh` (matrix runner), `pr135-perf.sh`
(profile capture), and earlier baseline/next-stage scripts. Inspect before reuse;
they contain this workspace's absolute paths and intentionally skip existing logs.
The matrix runner supports label, binary, window list, optional loss list and
optional direction list. Do not compile while taking measurements on the same host.
Some original combined logs include discarded/control-startup-failed attempts;
use the plan's table and valid separate-direction reruns as the source of truth.

The host filesystem filled while creating duplicate debug artifacts. Only this
task's `target/debug` was cleared; do not clear other user workspaces. Release
artifacts remain. Release validation used `CARGO_PROFILE_RELEASE_DEBUG=1` and
`CARGO_PROFILE_RELEASE_STRIP=false`; about 3.6 GiB remained at the last check.
Prefer `cargo test --release --lib` or manage this task's build caches to avoid
recreating large debug cdylib/staticlib artifacts.

No test/gateway/perf/build processes from this task remain running at handoff.
