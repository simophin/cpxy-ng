# OpenWrt GeoIP routing owned by cpxy

Proposed implementation plan, 2026-10-25. No runtime changes are made by this document.

Use Linux policy routing and destination routes, generated from cpxy's bundled GeoIP data.
cpxy owns their lifecycle. Do not add a separate OpenWrt GeoIP/PBR package or a downloaded
country-list service. Retain the existing `ip-full` and `kmod-tun` dependencies.

This replaces the OpenWrt nftables GeoIP-set classification proposed in
[the VPN geo-routing handoff](geo-routing-plan.md). Its Android design and future IP VPN
requirements remain separate work. Ordinary firewall rules remain necessary for forwarding,
DNS interception and refusal policies; they do not classify countries.

## Baseline and scope

Today `packaging/common/net.sh` creates a persistent TUN, a table and four rule priorities per
instance. All captured TCP reaches `mobile-engine`; its regional/private policy opens direct
sockets or uses the cpxy server. Router-originated sockets use normal system routing.

The first delivery is IPv4-only. It moves eligible direct TCP into kernel forwarding while keeping
the existing proxy transport, split DNS, per-network instances and IPv4 UDP policy. It does not make
the current TCP proxy an IP VPN. The same route compiler can later feed the full IP VPN.

While enabled, protected networks must refuse forwarded IPv6 internet traffic so it cannot bypass
the IPv4 policy. Preserve local IPv6 communication and necessary control traffic. No IPv6 GeoIP
routes, NAT66, prefix delegation changes or IPv6 tunneling are part of this delivery. Reject an
incompatible effective IPv6 setting before mutation with a clear configuration error; update the
router configuration/defaults and migration documentation rather than silently ignoring it.
Shared mobile/Tailscale IPv6 behavior is outside this change.

For that future VPN, retain the earlier plan's optional `router_instance` and explicit server
socket bypass. Router-originated capture needs its own marked-output design, server resolution
and bootstrap tests. Do not enable it as an incidental change to today's router package.

## Route design

Use each instance's policy table for both direct exceptions and the tunnel default:

```text
<eligible-direct-prefix-1> via <WAN gateway> dev <WAN device>
<eligible-direct-prefix-2> via <WAN gateway> dev <WAN device>
...
default dev cpxy<i> metric 10
unreachable default metric 1000
```

A normal destination takes the TUN default; an eligible direct destination takes its more-specific
WAN route. The unreachable default prevents non-direct traffic from falling through when the TUN
disappears. A terminal refusal rule after the table lookup also protects an empty/missing table.
See the Linux [route](https://man7.org/linux/man-pages/man8/ip-route.8.html) and
[policy-rule](https://man7.org/linux/man-pages/man8/ip-rule.8.html) documentation.

Resolve the effective uplink from OpenWrt/netifd and `main`, and generate gateway/device routes
with correct nexthop reachability, including point-to-point links.
Rebuild on WAN DHCP/PPPoE changes. Start with one effective IPv4 default;
ECMP and configurations needing another WAN policy table require explicit integration.
Do not claim automatic compatibility with arbitrary mwan3/PBR/VPN rule ordering.

Avoid regional `throw` routes followed by an unconditional `lookup main`: a missing policy
table would take that same continuation and leak non-regional traffic. Explicit WAN routes
cost uplink reconciliation but let all failed lookups terminate safely. If the uplink is absent,
represent direct prefixes as unreachable; do not silently send them through the future IP VPN.

The effective order for protected ingress is:

1. Kernel local-address handling and existing connected/local-network exceptions.
2. Existing DNS handling and protocol/port restrictions.
3. Existing ordinary-IPv4-UDP handling.
4. Lookup the instance table for captured traffic.
5. Terminal refusal if the instance lookup has no route.

Scope rules to the instance's LAN devices. Keep return routing from its TUN separate: do not
send return packets through the GeoIP decision again. Keep the existing local-network exception
precedence; forced proxy prefixes override GeoIP/private classification after those exceptions.

Missing *tables* are different from missing TUN devices: an empty/missing table falls through.
Keep an independent terminal protection rule throughout setup, switching and recovery, and verify
the active table's defaults before activating it. Final rule/table allocation
must reserve staging resources and validate collisions across instances and existing kernel state.
Do not assume the current four-priority allocation remains sufficient.

## Compile one routing policy

Reuse `geoip-data`'s embedded IPv4 `CN`, which already drives split DNS and engine routing.
Convert inclusive address ranges into exact, minimal CIDR coverage without widening membership.
For the checked-in snapshot, measured coverage is:

| Family | Input ranges | Exact collapsed CIDRs |
|---|---:|---:|
| IPv4 | 5,424 | 8,331 |

Compute eligible direct space as `(regional union existing private ranges) minus proxy_route`.
Subtract forced-proxy space before emitting WAN routes. Simply adding a broad forced-proxy
route would lose to a more-specific regional prefix under longest-prefix matching.

Share this compiler and dataset identity with the existing policy code. Add a route-export CLI
mode to `cpxy-router` that can run without `SERVER`, DNS listeners or a TUN worker. Emit validated
commands for `ip -batch`; do not start a process for every route. Shell lifecycle code consumes
the output. Direct rtnetlink can follow if measurements justify its maintenance cost.

Ship updates with the package initially. Record dataset hash, policy hash, effective prefix
counts and active generation in runtime status. No background download/update service is needed.
Changing the data must update DNS, userspace policy and kernel routes together.

## Preserve behavior at the forwarding boundary

- **DNS:** keep per-instance interception and split DNS. Classification uses the actual
  destination IP, including cached answers and literal IP connections.
- **Refusals:** direct forwarding bypasses the engine's UDP/443 and TCP/853 checks. Preserve
  applicable IPv4 refusals with owned firewall rules before enabling direct bypass.
  Do not let regional WAN routes bypass QUIC refusal.
- **UDP:** preserve today's ordinary-UDP path. General UDP tunneling belongs to the future IP VPN.
- **IPv6 exclusion:** install owned refusal rules for protected IPv6 internet forwarding before
  activating the IPv4 policy. This includes ordinary UDP, not just TCP or DNS. Preserve local
  access/control traffic and verify dual-stack applications fall back to IPv4. Suppressing AAAA
  answers alone is insufficient: clients may have cached answers or use literal IPv6 addresses.
- **Forwarding/NAT:** validate source-zone access to WAN and IPv4 masquerading. The engine's
  direct sockets previously used router output, so direct forwarding may need owned, narrowly
   scoped permissions. Preserve explicit administrator denies and leave IPv6 addressing untouched.
- **Visibility:** direct flows no longer reach the engine's connection log/API. State this in
  the UI/docs; report route status separately. Per-flow kernel traffic logging is separate work.
- **Failure/stop:** worker death retains protection for proxied traffic; direct traffic can
  continue. Preserve today's explicit service-stop behavior, which removes cpxy routing and
  restores ordinary connectivity.

## Lifecycle and failure handling

Validate the complete configuration before changing running state. Serialize reconciliation.
Build a new table generation off-path, install both protection defaults, load the complete
prefix batch, and check success/counts before switching an instance to it. `ip -batch` is not
an atomic transaction; never expose a partially loaded generation.

Switch under the independent protection rule so interruption can only interrupt connectivity,
not send tunnel-bound traffic to WAN. Keep the previous generation until the replacement is
verified. On failure retain the old policy or protection, report the error, and retain enough
state for cleanup. An unchanged reload must leave workers, connections and routes alone.

Reconcile reboot, LAN device changes, firewall reloads and relevant uplink events. Recheck
active route state even if the saved configuration is unchanged. Reserve and persist ownership
metadata; remove only owned rules/tables on instance deletion, reassignment, stop or uninstall.

Policy changes can move existing TCP flows between a router socket and native forwarding,
changing their source/NAT state. Define these changes as reconnecting affected flows. Check
conntrack and software/hardware offload behavior and invalidate affected state where needed;
do not promise seamless migration or permit offload to retain a newly forbidden direct path.

## Delivery sequence and acceptance

1. **Policy compiler:** exact range-to-CIDR conversion, private policy, forced-proxy subtraction,
   IPv4 export, dataset/policy identity. Test boundaries and equivalence with userspace
   classification, including broad proxy prefixes covering narrower regional ranges.
2. **Kernel lifecycle:** extend `net.sh`, init planning/state, collision validation and status;
   stage/switch/rollback generations and guard interrupted startup/reload. Keep bypass opt-in
   until validation is complete. Test command failures at each mutation boundary.
3. **Firewall and packaging:** preserve refusals, verify native WAN forwarding/NAT and update
   both ipk/apk payloads and removal hooks. Exercise fw3 and fw4 paths.
4. **Namespace integration:** extend `packaging/openwrt/test/lab.sh` with controlled regional
   destinations. Verify regional TCP reaches WAN without entering TUN or opening an engine
   socket; non-regional and forced-proxy TCP reaches the correct instance's server. Cover two
   instances, IPv6 bypass prevention, DNS, port refusals, missing TUN/table/default, worker death, interrupted
   reload, WAN changes, instance removal and unrelated route preservation.
5. **Booted OpenWrt integration:** run the package in an isolated VM with real procd, netifd,
   dnsmasq and firewall, using the test topology and release gates below.
6. **Hardware rollout:** on the GL-MT3000 measure route memory, batch load/reload time, CPU,
   direct throughput and latency against the current engine path. Test reboot, WAN reconnect,
   firewall reload and offload behavior. Route counts are per instance; measure several instances
   before choosing limits. Then enable by default and update the operator/traffic guides.

Completion means direct regional TCP demonstrably avoids userspace, proxy destinations retain
their assigned exit during failures/reloads, policy restrictions remain effective, and no extra
OpenWrt GeoIP/PBR package is installed.

## Test environment and release gates

QEMU is a suitable VM runner, not a requirement of the routing design. Use KVM acceleration
when available and software emulation otherwise. Boot actual OpenWrt x86-64 images; containers
remain useful for packaging checks but do not replace testing OpenWrt's own running kernel and
service lifecycle. See [OpenWrt's QEMU guide](https://openwrt.org/docs/guide-user/virtualization/qemu)
and [QEMU networking options](https://www.qemu.org/docs/master/system/qemu-manpage.html).

Use isolated virtual links, with LAN/guest clients and a controlled WAN/server network in Linux
namespaces or small helper VMs. A separate management link/serial console remains available
during deliberate network failures. Do not bridge the test LAN onto the real household LAN.

```text
LAN client ------- LAN NIC   OpenWrt VM   WAN NIC ---- controlled WAN ---- destinations
guest client ----- guest NIC     |                         |
                                TUN                 cpxy server(s)
```

Run the real router/server binaries and install the built ipk/apk. Pin firmware images and
checksums, and supply dependencies matching their kernels. Test a supported fw3/opkg image and
fw4 images covering the supported opkg/apk formats; physical GL.iNet firmware still needs its
own final check. Use disposable disk overlays so every run starts from known state.

The staged checks are:

1. **Compiler tests:** exact CIDR coverage, boundaries, private ranges, overlapping/broad forced
   proxy routes and invalid input. Compare generated membership against the existing classifier.
2. **Namespace lab:** real kernel routes, packets and binaries, with fast fault injection.
   Use a tiny explicit policy fixture for exhaustive cases plus the full production dataset for
   loading and representative route probes. Bind representative public addresses only inside
   the isolated lab, so assertions need no public servers or external GeoIP service.
3. **OpenWrt VM:** real UCI/procd/netifd/firewall/DNS behavior, boot/reboot, package upgrade/remove,
   unchanged and invalid reloads, two instances, DHCP gateway changes, link loss/recovery and
   PPPoE uplink coverage. Repeat route/refusal assertions after every lifecycle transition.
4. **Failure matrix:** suspend/kill the worker; delete TUN, active routes, whole table or default;
   interrupt route loading/switching; reload the firewall during traffic; change forced-proxy
   policy with established flows and software offload. Probe continuously through transitions
   for packets escaping to WAN, including IPv6 TCP/UDP from a globally addressed test client.
5. **Hardware gate:** after automated checks pass, test the GL-MT3000's actual firmware, ARM64
   binary, Wi-Fi/bridge integration, hardware offload and resource/performance limits. VM timings
   do not establish router throughput. Prepare a rollback package/config before deployment.

Success requires packet captures and destination-side source-address observations, not just
successful HTTP requests or `ip route get`. For regional TCP, prove packets traverse WAN without
entering the TUN or opening an engine socket. For proxy TCP, prove the destination observes the
correct server exit and sees no direct client connection, including during failures. Verify DNS,
QUIC/DoT refusal, ordinary UDP behavior, IPv6 internet refusal and retained local access separately.

Save route/rule dumps, firewall configuration/counters, service logs, packet captures, package
and image identities, and machine-readable assertions for failed runs. No real-router deployment
until compiler, namespace, packaging and booted-OpenWrt gates pass; report any unsupported test
environment explicitly instead of treating a skipped check as success.
