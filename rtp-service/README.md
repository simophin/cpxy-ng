# cpxy-rtp: HTTP-controlled RTP transport

The Linux service carries IPv4 through allocation-scoped, authenticated UDP
frames. Plain HTTP carries encrypted JSON-RPC control; it does not use TLS.
The client creates a dedicated UDP allocation and rotates it every five minutes.
There is no base UDP listener, fallback port or server rotation epoch.

This is a matched v2 client/server breaking change. Mobile integration is
separate: mobile and the old experiment worker return an explicit unsupported
protocol error. See the [wire contract](../docs/rtp-http-protocol.md) and
[validation report](../docs/rtp-http-validation.md).

## Run

Build with `cargo build -p rtp-service --release`. Linux needs TUN, iproute2,
and the privileges to create its interface, routes and scoped admission rules.
Set the same password and server identity at both ends. The password is read
only from `CPXY_RTP_PASSWORD`, removed from the process environment after use,
and derived with Argon2id (19 MiB, two passes). It never appears in argv.

```sh
CPXY_RTP_PASSWORD=... cpxy-rtp endpoint --listen 203.0.113.10:30000 \
    --udp-ports 30001-30100 --service-cidr 10.90.0.0/24

CPXY_RTP_PASSWORD=... cpxy-rtp client --entry 203.0.113.10:30000 \
    --outbound-interface eth0 --dynamic-address --service-cidr 10.90.0.0/24
```

`--listen` and `--entry` identify the **TCP HTTP control** port. The administrator
must admit that TCP port; UDP rules are managed separately per allocation.
With `--firewall none`, the administrator also provisions UDP admission and
forwarding. No code opens the entire pool or changes permanent firewall policy.

The endpoint's `--address` defaults to `10.88.0.1/24`. Dynamic addressing defaults
to usable addresses in that subnet, excluding server/network/broadcast, static
reservations and existing claims. Configure `--pool-low`, `--pool-high` and
repeat `--reserved-address` to separate the dynamic pool from static clients.
The pool is bounded to 1048576 addresses. A static client omits `--dynamic-address`
and uses `--address 10.88.0.20/24`; create records its claim atomically before a
dynamic assignment can collide. Address conflicts are errors, never route theft.

Each client persists its UUID in the state directory, separate from transient
leases. The server persists UUID/address mappings indefinitely: disconnect and
UDP expiry do not recycle an address. Back up this state. A copied UUID shares
one identity and its two-allocation quota; lost/corrupt state is diagnosed.
Live transport secrets and packet counters are never persisted. New creates
return an authenticated activation floor so a restarted client can supersede a
stale route without reusing keys/counters. Abandoned candidates may still require
lease expiry before another allocation fits the quota.

## Configuration and operation

Flags also have `CPXY_RTP_*` environment forms; consult `--help`.

| Setting | Default / meaning |
| --- | --- |
| `--server-identity` | `cpxy-rtp`; identical at both peers, bound into control AEAD |
| `--udp-ports` | Eight UDP ports after the TCP port; explicit LOW-HIGH recommended |
| `--state-dir` | `/var/lib/cpxy-rtp`; private durable identity/address/lease journals |
| `--interface` | `rtp0`; refuses an existing administrator interface |
| `--address` | Endpoint `.1/24`, static client `.2/24` in `10.88.0.0/24` |
| `--keepalive-seconds` | 15, jitter +/-20%, validated range 5..45 |
| `--firewall` | `auto`; firewalld when available, otherwise administrator managed |
| `--service-cidr` | Selected forwarding destinations; no public exit by default |
| `--internet-exit` | Explicit IPv4 exit; router routing/NAT provisioned externally |

Rotation is activation+300 s, with preparation 10 s early and a 10 s old-path
drain. Startup deadline is 30 s, authenticated inactivity expiry 120 s. Health
probes also detect missing inbound traffic during uploads: waits 1/2/4 s, one
outstanding probe, matching PONG only. Unknown HTTP outcomes are retried with
the same operation and fresh envelope/path. Preparation failures retain the
working path. New sockets and HTTP connections bind the physical device before
connect; namespace mode obtains both TCP and UDP descriptors from its host broker.

One TUN reader dispatches downstream to one bounded FIFO per client. It expires
packets after one second, limits each client to 256 packets/256 KiB and all
clients to 16 MiB, and sends at most 32 packets per allocation per turn. Inner
MTU remains 1200: maximum outer size is 1260 bytes over IPv4 or 1280 over IPv6.
This is RTP-shaped transport, not SRTP or a traffic-camouflage claim.

Peers sharing the PSK are trusted members. UUIDs are consistency identifiers,
not credentials. AEAD and replay windows protect against outside attackers;
this design does not provide forward secrecy or isolation between PSK holders.

## Firewall and routing

Managed firewalld uses generation-scoped UDP leases through `port-admission`.
Prepare/query/commit completes before returning an allocation. HTTP/firewall
work runs separately from packet processing; no packet-table lock spans D-Bus
I/O. Failed retirement retains the bound socket until reconciliation succeeds.
Journals contain ownership metadata, not keys. Reload/backend recovery is checked
every second. The service never reloads the host firewall or adopts identical
administrator rules. The TCP control port remains administrator provisioned.

For selected-network forwarding, the service binds its own TUN to the configured
runtime zone and optionally masquerades tunnel-subnet traffic to service CIDRs.
Configure host IPv4 forwarding explicitly; the service never toggles it.
Allocation framing enforces each client's inner address before forwarding;
selected destination policy remains enforced separately. Endpoint return routes
cover the configured tunnel subnet so dynamically assigned clients work.

Client selected routes use reserved tables/rule priorities with an unreachable
guard. Process death or failed startup keeps the guard; explicit stop/`cleanup`
releases it. `cleanup-ports` removes only admission leases, preserving guards.
Unrelated routes, rules, interfaces and permanent configuration remain owned by
the administrator. DNS remains external fail-closed policy metadata only.

```sh
cpxy-rtp endpoint ... cleanup-ports
cpxy-rtp endpoint ... cleanup
cpxy-rtp client ... doctor
```

`client ... namespace -- COMMAND` creates the TUN/routes inside an unprivileged
namespace and starts COMMAND after connection (90-second startup bound). Its
host parent handles only protected socket descriptors. It never sees keys or
inner packets. Every recovery connection still uses host networking.

`--listen '[::]:PORT'` supports both outer families; `--entry '[2001:db8::1]:PORT'`
uses an IPv6 outer path. Inner IPv6 remains unsupported; no host-wide IPv6 setting
is changed. A full-tunnel deployment must separately prevent IPv6 bypass within
its protected scope.

## Verification

```sh
cargo test --locked -p rtp-l3 -p rtp-service -p port-admission -p rtp-mobile
cargo test -p rtp-service five_minute_rotation -- --ignored
cargo check --workspace --locked
cargo build -p rtp-service -p port-admission -p rtp-mobile --bins
unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
RTP_TEST_REPAIR=1 unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
unshare -Urnm bash rtp-service/tests/linux-applications.sh
bash rtp-service/tests/linux-namespace-broker.sh
bash rtp-service/tests/linux-dual-stack.sh
unshare -Urnm bash rtp-service/tests/linux-firewalld.sh
bash rtp-mobile/tests/linux-lab.sh
```

The firewalld fixture runs on a private bus in disposable namespaces. Its
administrator-owned TCP admission is separate from service UDP ownership.
Historical mobile-v1 fixtures are archived and are not v2 acceptance evidence.
If the host lacks a TUN device, the optional root wrapper creates one entirely
inside private namespaces:

```sh
sudo unshare -nm bash rtp-service/tests/private-linux-lab.sh rtp-service/tests/linux-lifecycle.sh
```

It refuses the host network/mount namespace, stages only the executable in a
private temporary directory, and removes it on exit.
