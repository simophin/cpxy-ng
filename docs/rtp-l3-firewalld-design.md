# RTP service: firewalld port lifecycle

Status: A2 implemented with the limits recorded below; see the [application plan](rtp-l3-application-plan.md).
Dynamic UDP rules belong to the service supervisor, coupled to socket and
rotation ownership. Packet workers do not execute firewall commands.

## Adapter and authority

Use the system D-Bus runtime interface `org.fedoraproject.FirewallD1.zone`:
`addRichRule`, `queryRichRule` and `removeRichRule`; resolve/validate the configured
zone/interface before preparing a port. Rich rules can scope source, destination
and UDP port. D-Bus also exposes `Reloaded`; watch the firewalld bus owner's
replacement for daemon restarts. These API facts are documented in the official
[D-Bus manual](https://firewalld.org/documentation/man-pages/firewalld.dbus.html)
and [rich-language manual](https://firewalld.org/documentation/man-pages/firewalld.richlanguage.html).

The administrator delegates an explicit bind-address/port-pool/zone namespace
to one local lease manager. A small privileged agent validates every request
against that policy. Workers receive sockets/TUN descriptors and cannot submit
arbitrary rules, shell commands, zones or destinations. Serialize multiple
profiles through that manager. Keep permanent configuration and unrelated rules
outside its ownership.

The endpoint admits configured client ingress CIDRs to its live UDP ports.

Do not assume a production client's address is the PoC's measured /32. Admission
CIDRs are explicit profile policy, potentially public admission when deliberately
configured. Never expand firewall policy from an unauthenticated UDP request.

## OwnedPortLease contract

Each software lease records profile, owner/boot generation, lease ID, bind IP,
numeric port, UDP protocol, exact canonical rule(s), zone, socket handle and
state. The journal is written atomically before mutations and reconciled after
restart. It contains no VPN keys or port secrets.

```text
prepare(spec, generation) -> lease ready | error
commit(lease)             -> active
retire(lease)             -> draining -> released
release(lease)            -> idempotent cleanup
reconcile(desired)        -> actual owned inventory + health
```

Use runtime rules with timeout zero for committed service sockets in the first
implementation. Software leases are supervised; they are not native firewalld
ownership tokens. Cleanup on worker death, supervisor restart and service stop
is mandatory, including an independent systemd stop/reconciliation action after
an ungraceful exit. On reboot runtime rules disappear. A lone timeout-zero rule
plus best-effort destructors is not a complete implementation.

Timed rules may later bound preparation windows. Re-adding an existing rule is
not assumed to refresh a timer. Do not implement active-rule renewal by removing
and re-adding the only admitting rule; that creates an avoidable outage window.
Keeping live rules untimed separates service liveness/ownership from rule TTLs.

## Prepare, advertise and retire

1. Reserve a free port in the configured pool and bind its socket. Keep the
   current path serving; do not increment epoch or announce the candidate.
2. Journal intent and inspect the exact admission rules. Refuse ambiguous
   ownership. A preexisting identical rule is not automatically ours to delete;
   require an explicit administrator-managed policy or report the conflict.
3. Add only missing owned runtime rules and query them. Readiness means the
   socket and required rules are present.
4. Commit the endpoint state/secret and only then advertise STATE. A bounded
   staged socket may temporarily exist in addition to base/current/previous.
5. On any preparation failure, release only this attempt's resources and keep
   the existing epoch/path. An ambiguous D-Bus timeout triggers reconciliation,
   not an unconditional second mutation or a success assumption.
6. Retain previous ports under the protocol's grace/session rules, including
   its explicit oldest-port eviction bound. Retire the exact lease generation;
   remove its owned rule before releasing the bound port for another service.

Base is prepared before registration becomes available. It can carry fallback
data as well as control. A configured rotation pool is an allocation boundary,
not permission to expose the entire range permanently.

## Reloads, crashes and errors

Runtime-only changes can be lost on reload, so the manager must reconcile after
reload/startup. See the official
[firewall-cmd runtime/reload documentation](https://firewalld.org/documentation/man-pages/firewall-cmd.html).
This application does not issue a host-wide reload to add or remove a port.

- Subscribe, enumerate and reconcile at startup; do not rely on signals alone.
  Recheck before reporting readiness and periodically while running.
- A reload marks admission readiness degraded until owned live leases are
  restored and verified. Freeze new rotations while restoration is uncertain.
  Existing sockets remain open; a reload need not guarantee uninterrupted data.
- If firewalld is unavailable at managed-mode startup, fail readiness. An
  explicit administrator-managed mode may verify externally managed admission;
  never silently switch to it after a D-Bus failure.
- Use bounded D-Bus calls and a separate control task; UDP polling continues
  while operations are pending. Preserve backend errors in structured health.
- Journal and generation checks prevent an old process/retirement acknowledgement
  deleting a port that a newer instance has acquired. Reconcile only the delegated
  namespace, not every UDP rule in a host zone.

Normal worker shutdown releases leases. Forced worker/supervisor termination
must run the independent journal cleanup; after supervisor restart, remove stale
owned entries and rebuild desired leases before readiness. Never flush a zone,
change the default zone, save runtime to permanent, or put an entire TUN into
`trusted` as the general application policy.

TUN forwarding is a separate scoped deployment policy: client pool to approved
backend service CIDRs, permitted return traffic and optional explicit SNAT.
Required host forwarding is provisioned deliberately, rather than toggled on
and off whenever a worker starts or stops.

## A2 completion checks

Fake-adapter fixtures cover each failure/timeout boundary, repeated release,
preexisting rules, generation reuse and cancelled preparation. A real isolated
firewalld fixture covers rotation, reload/daemon restart, forced process death,
two profiles, rule-query verification and unrelated runtime/permanent state.
Assert that unprepared ports are never advertised, current traffic survives a
failed preparation, and stopped/crashed services leave no owned open rules after
supervisor reconciliation. No live host is modified by this design document.

## Implemented A2 scope (2026-10-25)

`port-admission` owns the shared lease contract, separate policy-validated local
agent and canonical-rule journal; it has no RTP dependency. The service uses
SO_PEERCRED-authenticated Unix RPC. The agent uses bounded `busctl` calls to the
system D-Bus API, not shell commands or firewall-cmd. Each numeric-port allocation
has a distinct generation. Identical preexisting rules fail preflight without
being adopted. Agent and profile journals survive ambiguous errors/restarts.

The actual worker remains a thread inside the privileged TUN-owning service;
this does not establish an OS privilege sandbox. Reload and owner loss are
detected through one-second periodic reconciliation rather than subscriptions.
The configured sources/policies and request sizes are bounded. Scheduled
prepare/commit, reconciliation and rule retirement run on separate control tasks;
old sockets remain held until retirement succeeds. Initial base/candidate setup
finishes before readiness. Task drops join before cleanup to avoid late mutation.

Real disposable firewalld/TUN verification and installation details are recorded
in the application plan and `port-admission/README.md`. Independent systemd
ExecStopPost cleanup must run while the agent and firewalld are available. If all
supervisors are lost, untimed rules persist until restart/explicit reconciliation;
failed cleanup is retained in the journals. No automatic host deployment or
public-network acceptance is implied.
