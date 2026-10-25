# cpxy-rtp: managed Linux RTP service

`cpxy-rtp` runs one end of the RTP-framed IPv4 tunnel. An **endpoint** accepts
clients over UDP and forwards their traffic to selected private networks (service
CIDRs). A **client** connects to an endpoint, or to a
[generic UDP relay](../udp-relay/README.md) in front of one. It is not an
internet VPN: only the configured service CIDRs are routed, and IPv4 with
MTU 1200 is the only mode.

## Running an endpoint

CI builds static musl binaries for x86_64 and aarch64 (the `cpxy-rtp-<target>`
artifacts, which also contain `cpxy-udp-relay`). They have no runtime
dependencies: copy one to a Linux host with `/dev/net/tun`, iproute2 and a public
IPv4 address, and run it as root (it owns a TUN, routes and firewall rules):

```sh
CPXY_RTP_PASSWORD=... cpxy-rtp endpoint --listen 203.0.113.10:30000 \
    --service-cidr 10.90.0.0/24
```

Enable IPv4 forwarding (`net.ipv4.ip_forward=1`) first; the service never changes
it. With firewalld running, nothing else is needed: see [Firewall](#firewall).
Without it, open the listen port and the rotation port range for UDP yourself,
allow forwarding from the TUN to the service CIDRs, and make sure backends can
route replies back (a route to the tunnel pool via this host, or your own SNAT).
Run it under whatever supervisor you use; it stays in the foreground and logs
JSON events to stderr. After a crash, run `cpxy-rtp endpoint ... cleanup-ports`
(release ports, keep routing guards) or `cleanup` (release everything) with the
same settings.

A client on Linux uses the same binary:

```sh
CPXY_RTP_PASSWORD=... cpxy-rtp client --entry 203.0.113.10:30000 \
    --outbound-interface eth0 --service-cidr 10.90.0.0/24
```

`cpxy-rtp client ... namespace -- COMMAND` instead runs COMMAND in an
unprivileged user/network namespace whose only route is the tunnel; see
[Namespace client](#namespace-client).

## Configuration

Everything is a flag, and every flag also reads a `CPXY_RTP_*` environment
variable (`cpxy-rtp endpoint --help` lists both). There is no configuration file
and no reload: change the environment and restart. SIGTERM, SIGINT and SIGHUP
stop gracefully.

The shared password is read **only** from `CPXY_RTP_PASSWORD`, never from a flag,
so it stays out of process listings. It must be at least 12 characters. Both
sides derive the 32-byte key with Argon2id (19 MiB, 2 passes) and a fixed
protocol salt, so a password change needs every client updated. The service
removes the variable from its environment after reading it, so namespace
applications never inherit it.

Commonly used endpoint settings, with defaults:

| Flag / variable | Default | Meaning |
| --- | --- | --- |
| `--listen` / `CPXY_RTP_LISTEN` | required | Public IPv4 and base UDP port |
| `--rotation-ports` | next 8 ports | Rotation pool `LOW-HIGH` |
| `--rotation-seconds` | 300 | 0 disables scheduled rotation |
| `--service-cidr` (comma-separated) | none | Networks clients may reach; enables routing policy |
| `--address` | `10.88.0.1/24` | Endpoint tunnel address; its network is the tunnel pool |
| `--client-address` (comma-separated) | `10.88.0.2` | Client tunnel addresses allowed in |
| `--interface` | `rtp0` | TUN name; must not already exist |
| `--state-dir` | `/var/lib/cpxy-rtp` | Lease, routing and firewall journals; created 0700 |
| `--route-table`, `--rule-priority` | 18800, 1000 | Reserved per instance; see [Routing](#routing) |
| `--firewall` | `auto` | `auto`, `firewalld` or `none` |

Each process uses the fixed identity `cpxy-rtp` for its leases and firewall
rules, and as its relay management owner. Run one endpoint per state directory.

`cpxy-rtp endpoint doctor` prints read-only diagnostics as JSON: whether the
password variable is set, TUN availability, socket binding, the firewalld policy
it would use, and the routing and DNS policy. It never prints the password.

## Firewall

`--firewall auto` (the default) manages firewalld in-process when it answers on
the system bus, and otherwise logs that UDP admission is the administrator's job.
`--firewall firewalld` fails startup when firewalld is absent; `none` never
touches it. The startup `admission` event says which mode is in effect. All
changes are runtime-only: permanent configuration is never modified, and a
`firewall-cmd --reload` or firewalld restart is repaired within about a second
by periodic reconciliation.

When firewalld is managed, the endpoint:

- opens exactly the listen port and each rotation port as it is used, as rich
  rules restricted to `--firewalld-source` CIDRs (default `0.0.0.0/0`, at most 4)
  and the listen address, in the zone of the listen interface (`--firewalld-zone`
  and `--firewalld-interface` override the lookup). A port whose identical rule
  already exists is refused instead of shared, and removed rules always belong
  to this service.
- binds its own TUN to `--firewalld-tunnel-zone` (default `trusted`) so firewalld
  forwards decrypted packets. The service's packet allowlist still limits them
  to configured client addresses and service CIDRs. Administrator interfaces
  are never reassigned.
- adds, unless `--no-masquerade`, one rule per service CIDR,
  `source <tunnel pool> destination <cidr> masquerade`, in the zone of the
  interface that routes to that CIDR. Backends then need no return route but
  see the endpoint's address rather than the client's.

Every owned rule and binding is journaled before it is applied. A normal stop
removes them; after a crash, `cleanup-ports` releases ports and `cleanup`
releases ports, forwarding and routing policy.

## Routing

`--service-cidr` installs policy rules at `--rule-priority` that send those
destinations to the reserved `--route-table`, which holds the live TUN routes
and an unreachable default. If the TUN disappears, the guard keeps rejecting
traffic instead of falling through to a physical route. Startup refuses table
or rule collisions and earlier custom rules. Normal stop and `cleanup` release
the guard; a crash, failed startup or `cleanup-ports` deliberately keep it until
restart or explicit cleanup. Cleanup never flushes a table or replaces
administrator routes. A reboot removes kernel policy, so start the service
before applications that need guarded access.

On the endpoint, inner packets are accepted only from listed client addresses to
the service CIDRs; on a client, only from the service CIDRs to its own address.
The existing experimental protocol uses one shared key and unauthenticated data
encryption: it provides no per-client cryptographic identity. Use separate
endpoint instances (and passwords) when clients must be isolated from each other.

DNS is metadata only: `--dns-resolver` (inside a service CIDR) with
`--dns-suffix` records the private resolver, and the administrator must provide a
persistent fail-closed local resolver that never retries those suffixes publicly.
The service never changes DNS.

## Relays

A plain relay with fixed mappings needs nothing from the endpoint. For a relay
whose listeners follow the endpoint's port rotation, pass `--relay-management`
with the relay's mTLS management address and the `--relay-server-name`,
`--relay-ca`, `--relay-certificate`, `--relay-private-key`, `--relay-listen-ip`
and `--relay-endpoint-ip` flags (all required together); the relay's
`authority.owner` must be `cpxy-rtp`. Each base, initial and
rotating port is then prepared and committed on the relay before the endpoint
advertises it; see the [relay README](../udp-relay/README.md).

## Namespace client

`cpxy-rtp client ... namespace -- COMMAND` needs unprivileged user, network and
mount namespaces. The host-side parent keeps a private socketpair and creates
every outer UDP socket in host networking, bound to `--outbound-interface` and
connected only to the configured entry IP; it never handles keys or tunnel
packets and never changes host routes or firewall. TUN and routes live in the
child namespace. COMMAND starts once connected (90-second limit) and its exit
stops the service; without COMMAND it runs until a signal.

## Verification

```sh
cargo test -p port-admission -p rtp-l3 -p rtp-service --locked
cargo build -p rtp-service -p udp-relay -p port-admission
unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
RTP_TEST_REPAIR=1 unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
unshare -Urnm bash rtp-service/tests/linux-applications.sh
unshare -Urnm bash rtp-service/tests/linux-firewalld.sh
bash rtp-service/tests/linux-namespace-broker.sh
```

The namespace tests need TUN, iproute2, ping, Python 3 and unprivileged
namespaces; the firewalld test also needs firewalld, dbus-daemon and busctl, and
runs a private firewalld on a private bus without touching the host firewall. It
covers auto-detection, owned and conflicting rules, the TUN zone binding and
masquerade to a backend with no return route, a hung firewalld, reload and
restart recovery, SIGKILL plus repeated independent cleanup, graceful-stop
cleanup, and failing closed when firewalld is required but absent.
