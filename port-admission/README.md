# Delegated UDP admission

This crate has no RTP dependency. `Admission`, `LeaseManager` and the local
`port-admission AGENT.json` agent can also serve a generic UDP relay.
The agent invokes the firewalld runtime D-Bus API through `busctl` with a
one-second call timeout. It never invokes a shell or firewall-cmd. Runtime
operations add/query/remove exact rich rules, without host-wide reloads,
permanent changes or broad port ranges.

The administrator supplies the agent policy (example in `examples/agent.json`):
profile, exact IPv4 bind address, base and rotation pool, effective interface zone
and one to four explicit source CIDRs. An unassigned interface resolves through
firewalld's default zone. Up to 32 profiles have disjoint port namespaces. This
namespace delegation must be exclusive: another manager must not concurrently
add/remove identical rules. Existing identical rules cause a conflict and are
never adopted or deleted. IPv6 is outside this initial adapter.

The agent owns a separate locked, atomic, fsynced journal containing canonical
rules and allocation generations. It records intent after preflight and before
mutation; ambiguous add failures remain recoverable. Repeated release is safe;
a stale generation cannot remove its replacement. Query/add/remove failures stay
errors. A service restart or independent cleanup uses its own durable journal to
retire only the recorded generation, then rebuilds before readiness. Agent
restart preserves its inventory, allowing live services to reconcile rather
than tearing down their working rules.

Run the agent as the identity authorized by system D-Bus/polkit to change
firewalld. The Unix socket permits one configured `allowed_uid` (SO_PEERCRED);
the agent chowns the socket to that UID with mode 0600. Its root-owned parent
directory must allow that UID to traverse it, while policy and agent journal
must remain administrator-owned. The unit template creates a traversable
RuntimeDirectory and private StateDirectory, and removes the stale socket only
after agent exit. Copy/install templates and binaries explicitly; no host service
is installed by building. RTP units using managed admission should have a drop-in
`Requires=cpxy-port-agent.service` and `After=cpxy-port-agent.service`, and retain
the supplied independent ExecStopPost cleanup. Stop RTP profiles before stopping
the agent; cleanup requires firewalld and the agent to be available. Failed
cleanup remains journaled for retry. If both supervisor and its service manager
are killed, rules persist until restart/explicit cleanup; no timeout claim is made.

The current RTP service still owns privileged TUN setup and packet worker threads
in the same process. A separate agent restricts its firewall RPC to delegated
ports but does **not** establish an OS privilege sandbox for that worker. Running
the worker under a distinct UID and brokering all privileged descriptors is a
separate deployment/implementation step.

RTP base and initial candidate preparation completes before readiness. Scheduled
prepare/commit, retirement and one-second periodic reconciliation run outside
packet polling. A candidate is advertised only after verified admission; old
sockets remain held until rule removal succeeds. Reload/daemon loss is detected
by periodic queries (up to one second plus bounded calls, not D-Bus signal
subscriptions). Failed reconciliation degrades health and freezes rotation until
all leases are restored. Existing UDP polling continues during pending calls;
reload itself can interrupt admission until restoration. Each individual D-Bus
call is bounded; an operation may perform several calls for its bounded source
and lease inventory.

Checks:

```
cargo test -p port-admission -p rtp-l3 -p rtp-service
cargo build -p port-admission -p rtp-service
unshare -Urnm bash rtp-service/tests/linux-firewalld.sh
```

The last fixture uses a private system bus and real firewalld in disposable
network/mount namespaces. It requires Linux user namespaces, iproute2, dbus,
firewalld, nftables, ping, Python and ripgrep. It does not change the host firewall.
