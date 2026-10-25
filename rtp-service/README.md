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
For an unprivileged isolated application use:

```sh
cpxy-rtp namespace-run PROFILE.json -- curl --fail http://10.90.0.2:8080/
```

This Linux mode requires unprivileged user/network/mount namespaces, TUN and
iproute2. It launches the same managed Rust service using `unshare -Urnm`.
The host parent retains a private unnamed Unix socketpair and creates every
initial, candidate and repair UDP socket in host networking. Each descriptor
uses the configured bind address and physical `outbound_interface`, and connects
to the configured entry IPv4 address at the requested nonzero port. Requests
cannot choose another IP, device or bind address. Use bind port `0` so overlapping
candidate sockets can obtain distinct source ports. Descriptor replies carry
validated port labels; allocation failure rejects only that candidate. The parent
never handles keys or tunnel packets and never changes host routes or firewall.
Only the child pair endpoint survives launcher exec; it is CLOEXEC again before
application launch. No broker pathname or public listener is created.

The client profile must use `administrator_managed` admission. Its private journal
and key paths remain accessible inside the child mount namespace. TUN setup and
all optional profile routes stay inside the child network namespace. The optional
command starts after authenticated `connected`, with a 90-second connection
limit; command completion stops the service. Without a command, run until
SIGINT/SIGTERM. Termination/error stops and reaps the child process groups; the
nonpersistent TUN closes. SIGHUP restarts the same profile/key; configuration
changes require a fresh namespace run because the host broker policy is fixed.
This isolates networking and mount changes; it is not a filesystem sandbox for
untrusted applications. The ordinary `run` mode and library API remain available.

The key is exactly 32 bytes in a private regular file; symlinks are refused.
Create a private journal directory before startup. A journal has an exclusive
lock, generation-bound leases and fsync/atomic rename before mutations. Stale
leases are retired under that lock before a new generation starts. Failed
cleanup stays journaled and blocks successful startup until retried. Do not
share a bind-address/port namespace across profiles or use different journals
for the same profile. Journal paths are trusted administrator configuration.

The service owns a nonpersistent TUN, its address, MTU 1200, and link activation.
It refuses preexisting interface names. Descriptor teardown removes the TUN and
its connected route on failure/stop/process death. Optional `network` policy installs only selected IPv4 service routes. The service
never changes forwarding, NAT, or DNS. IPv4 is the only supported mode.

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
via `cleanup-ports` in ExecStopPost, including after SIGKILL; configure its paths
before installing. This admission-only cleanup deliberately preserves routing
guards after failure; normal SIGTERM stop releases the profile network policy.
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

For backend applications add the same `network` policy to the endpoint/client
profiles (use separate table/priority reservations for profiles on one host):

```json
"network": {
  "tunnel_pool": "10.88.0.0/24",
  "client_addresses": ["10.88.0.2"],
  "service_cidrs": ["10.90.0.0/24"],
  "table": 18800,
  "rule_priority": 1000
}
```

The endpoint address is `10.88.0.1/24`, client `10.88.0.2/24`, and MTU remains
1200. The configured pool must match the TUN address prefix. The endpoint accepts
inner packets only from listed client addresses to selected service CIDRs; the
client accepts returns only from selected services to its own address. This
allowlist restricts decrypted packet forwarding. The existing experimental
protocol uses a shared key and unauthenticated data encryption: it does not
provide independent cryptographic client identities or authenticated inner IP
packets. Use separately provisioned profile/key/endpoint namespaces when clients
must be mutually isolated. The profile name is the local tunnel identity.

Reserve an otherwise empty numeric table (10000..2000000000) and unused priority
(100..30000) per profile. Startup refuses table/rule collisions and earlier custom
policy rules, so selected CIDRs take precedence over routes in the main table,
including existing private/direct routes. Choose a priority preceding other policy
rules. The local kernel rule remains authoritative. Existing proxy profile code
and defaults are unchanged. No default/internet route is supported here.

Before startup administrators must provision endpoint IPv4 forwarding, scoped
forward-firewall admission, and backend return routes for the client pool via the
endpoint's backend interface. For the documentation subnet, the backend route is
`10.88.0.0/24 via 10.90.0.1`. Startup never enables global forwarding or adds NAT.
If return routes cannot be provisioned, an administrator may separately configure
explicit SNAT restricted to the client pool, service CIDRs and backend egress;
that policy and cleanup remain administrator-owned. This implementation/fixture
uses explicit return routing and does not claim SNAT verification.

Policy rules send selected destinations to a dedicated table containing the live
TUN routes and an unreachable default. When the TUN disappears, the durable guard
continues rejecting traffic instead of falling through to a physical default
route. A sidecar `<journal>.network.json`, protected by the existing profile lock,
records exact owned selectors with atomic fsync/rename before mutation. Restart
adopts guards without removing rules; SIGHUP keeps guards throughout its controlled
restart. Network/interface changes require explicit disconnect first. Do not
manually reuse that table, priority or journal while guards exist.

Normal stop and explicit `cleanup PROFILE.json` release the profile's rules and
guard. Failed startup/worker death and `cleanup-ports PROFILE.json` retain them;
this is intentional fail-closed behavior until restart or explicit disconnect.
Systemd's ExecStopPost uses `cleanup-ports`, so automatic crash cleanup cannot
expose selected traffic. Explicit cleanup restores administrator routing and may
therefore allow direct traffic after disconnect. Cleanup never flushes a table,
replaces administrator routes, or deletes an existing administrator interface.
Host reboot removes kernel policy; arrange service startup before applications
requiring guarded access. Protection assumes administrators do not change reserved
policy resources or install higher-precedence bypass rules while the profile runs.

Optional DNS is **externally managed metadata only**:

```json
"dns": {
  "mode": "externally_managed_fail_closed",
  "resolver": "10.90.0.53",
  "suffixes": ["corp.example"]
}
```

Place `dns` inside `network`. The resolver must lie in a selected service CIDR.
Administrators must configure a persistent local resolver that sends these private
suffixes only to this resolver and refuses public/default-resolver retry, including
after tunnel/process failure. This service neither installs nor verifies that
external DNS policy; doctor explicitly reports it unverified. Automatic DNS modes
are rejected. Ordinary per-TUN systemd-resolved configuration is insufficient for
this promise because its suffix state disappears with a crashed TUN. Automatic
DNS lifecycle support is deferred; no automated DNS success/no-leak claim is made.

`unshare -Urnm bash rtp-service/tests/linux-applications.sh` verifies TCP and UDP
from ordinary sockets to a third backend namespace, explicit return routing,
SIGKILL with a working physical fallback present, admission-only post-crash
cleanup, restart, SIGHUP, transport outage, unrelated route/rule preservation,
preexisting-interface failed startup, repeated explicit cleanup, and rejection
of unsafe automatic DNS settings. No public host deployment is implied.

`bash rtp-service/tests/linux-namespace-broker.sh` verifies this launcher with a
managed endpoint, a nested isolated client application, real ICMP over UDP across
repeated candidate socket replacements, TUN teardown and unchanged host IPv4
routes/rules. The fixture creates its own outer user/network namespace.
