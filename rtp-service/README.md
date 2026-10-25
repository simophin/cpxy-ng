# Managed Linux RTP service

`cargo build -p port-admission -p rtp-service --locked` builds `target/debug/cpxy-rtp`. It invokes
no Python launcher. The authoritative codec/session/recovery code and its 26
regressions now live in `rtp-l3`; the historical experiment imports that crate.
The existing experimental crypto/security properties are unchanged.

Use `cpxy-rtp run PROFILE.json`, `doctor PROFILE.json` and `cleanup PROFILE.json`.
SIGTERM/SIGINT cancel and join the worker; SIGHUP validates a fresh profile/key
and performs a controlled restart with a new session. Library callers use
`Service::{start,stop,reload,health}`. Starting returns a handle before path
establishment: endpoint `ready` means listener and initial rotation admission
are prepared; client health remains `starting` until authenticated candidate
validation and registration produce `connected`. Health also reports
`repairing`, `connected` after `repaired`, `degraded`, `failed` and `stopped`.
Events and diagnostics exclude PSKs and rotation secrets.

Endpoint example (documentation addresses only):

```json
{
  "profile": "example",
  "key_file": "/etc/cpxy-rtp/example.key",
  "journal": "/var/lib/cpxy-rtp/example.json",
  "interface": "rtp0",
  "address": "10.88.0.1/24",
  "transport": {
    "role": "endpoint",
    "listen": "192.0.2.2:30000",
    "rotation_low": 30001,
    "rotation_high": 30008,
    "rotation_seconds": 300
  },
  "keepalive_seconds": 15,
  "admission": "administrator_managed"
}
```

For a client set `address` to `10.88.0.2/24`, add the top-level
`"outbound_interface": "eth0"`, and replace `transport` with
`{"role":"client","entry":"192.0.2.2:30000","bind":"192.0.2.1:0"}`.
Every initial/replacement client socket uses SO_BINDTODEVICE on that physical
interface before connecting, preventing capture by routes through this TUN.
The key is exactly 32 bytes in a private regular file; symlinks are refused.
Create a private journal directory before startup. A journal has an exclusive
lock, generation-bound leases and fsync/atomic rename before mutations. Stale
leases are retired under that lock before a new generation starts. Failed
cleanup stays journaled and blocks successful startup until retried. Do not
share a bind-address/port namespace across profiles or use different journals
for the same profile. Journal paths are trusted administrator configuration.

The service owns a nonpersistent TUN, its address, MTU 1200, and link activation.
It refuses preexisting interface names. Descriptor teardown removes the TUN and
its connected route on failure/stop/process death. It does not install service
CIDRs, DNS, forwarding or NAT (A3). IPv4 is the only supported mode.

Use `"admission":"administrator_managed"` for externally provisioned UDP
policy, or `"admission":{"firewalld":{"agent_socket":"/run/cpxy-port-agent/agent.sock"}}`
for managed admission. `doctor` remains read-only and reports admission as
unverified outside a running service. The shared `port-admission` crate provides
the protocol-independent lease journal, privileged local agent and firewalld
adapter; see [its setup and authority documentation](../port-admission/README.md).
Binding and verified admission precede epoch changes/STATE; rule removal precedes
socket release. Failed retirement retains its socket and freezes rotation.
Reconciliation failure reports degraded health and freezes new rotation until
owned rules are restored. Each numeric-port allocation has a distinct generation.

The controller and worker use threads in one privileged TUN-owning process;
there is **no OS worker sandbox or privilege drop**. The separate agent's typed
Unix RPC validates the peer UID and administrator-delegated profile policy.
Base/initial admission finishes before readiness. Scheduled prepare/commit,
retirement and one-second periodic reconciliation run outside packet polling,
so pending firewall calls do not block the working path. Control tasks finish
before shutdown cleanup.
The portable state APIs already receive monotonic time; the Linux worker also
accepts a `Clock`, packet `PacketIo`, socket provider and structured event sink.

The optional systemd template in `deploy/` runs independent journal cleanup
via ExecStopPost, including after SIGKILL; configure its paths before installing.
It intentionally contains no host-specific endpoint or privilege policy.

Verification:

```sh
cargo test -p port-admission -p rtp-l3 -p rtp-service --locked
cargo build -p port-admission -p rtp-service --locked
unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
RTP_TEST_REPAIR=1 unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
unshare -Urnm bash rtp-service/tests/linux-firewalld.sh
```

The namespace fixtures need Linux TUN, iproute2, ping, rg, Python 3 (fixture assertions only) and namespace authority.
They isolate interfaces/routes/processes and verify real packet exchange,
rotation, reload, restart, empty journals and vanished TUNs. Repair mode keeps a
single ping process running across a blackholed active tuple, using the retained
experimental fault hook, and requires explicit repairing/repaired events.

The firewalld fixture additionally uses a private system bus, real firewalld and
nftables. It verifies failed D-Bus calls, preexisting-rule conflicts while traffic
continues, occupied ports, two profiles/generation reuse, pending control calls,
agent/firewalld restart and reload, unrelated rules and SIGKILL plus independent
cleanup. It never touches the host firewall.
