# IPv4 GeoIP router validation

Validated on 2026-10-26 on `feat/openwrt-ipv4-geo-routing`. Runtime implementation: `c91aff2`.
The feature is opt-in; no physical router was changed. The real router and cpxy server binaries
ran in isolated test networks. Unused client/tsgw executables in the focused VM package were
stand-ins, with those components disabled.

## Completed checks

| Check | Result |
|---|---|
| `cargo test --locked --offline -p client -p mobile-engine -p geoip-data` | 109 tests passed |
| IPv4 compiler equivalence | All regional range boundary neighbors plus 50,000 deterministic addresses agree with userspace policy; broad/overlapping proxy exclusions tested |
| Release musl builds | x86-64 and ARM64 passed |
| Namespace routing lab | 104 assertions passed, including the existing native-router checks |
| OpenWrt init/UCI regression | Passed, including missing LAN devices deferring activation, IPv6/offload rejection and stop/start migration |
| opkg container install/removal | Passed; unrelated configuration restored |
| Native apk metadata/architectures/install/upgrade/removal | Passed |
| OpenWrt 25.12 apk hook roundtrip | Passed, including disabled-service and configuration preservation |
| ShellCheck | Modified packaging/init/helper/test scripts passed at warning severity |
| Booted OpenWrt 24.10.0 / fw4 | Full suite passed |
| Booted OpenWrt 21.02.7 / fw3 | Full suite passed |

The VM tests use real procd, netifd, dnsmasq and the firmware firewall. QEMU ran with software
emulation, 256 MiB RAM and two virtual CPUs. Containers separately check packaging; they do not
substitute for the booted kernels.

Both firmware images also passed a strengthened IPv6 check: a temporary test-only bypass first
proves routed IPv6 connectivity without NAT66, then removing it produces an immediate policy
refusal. This avoids treating a missing IPv6 return route as successful protection.

## Firmware and data identity

Images and dependencies came from the official release directories, with SHA-256 verified
against their published indices:

| Image | Kernel / firewall | Compressed image SHA-256 |
|---|---|---|
| `openwrt-24.10.0-x86-64-generic-ext4-combined.img.gz` | 6.6.73 / fw4 | `a873d9f2e6e667d03dd06f75ed1f1b580beb7ce4337d5a44b60911a266965fb5` |
| `openwrt-21.02.7-x86-64-generic-ext4-combined.img.gz` | 5.4.238 / fw3 | `39f8e1f629cfe1db985809639c2cd1de88db119544144740d968d0b6784a9d6f` |

Router release binary SHA-256:

- x86-64: `da677bb1d7cd622a7b2dc3437e4f15bd3206515f50cbc65a7c176e1c02a5c6bf`
- ARM64: `a202a55d0d6a32e5071f6e61c862020eb4af734371908aceee63282cf871cd29`

GeoIP IPv4 data identity is
`e7059375cb244a9391df14b90782c34c10325321275d48cfe8e357649da17140`, shared by DNS and
userspace/kernel classification. The baseline has 8,331 regional CIDRs; adding private ranges
gives 8,336 eligible direct prefixes before proxy overrides. With WAN available, an active table
contains 16,674 routes: direct routes, their unreachable backups and two defaults. Staging briefly
requires a second generation per instance. These counts are not a hardware memory benchmark.

## Packet and lifecycle assertions

LAN and guest clients use separate cpxy server processes and observably different exit sources.
Controlled regional and non-regional endpoints live exclusively inside the isolated network.
Tests do not rely on public websites, external DNS or an external GeoIP service.

- Regional TCP succeeds with no packet worker or proxy server, proving native kernel forwarding.
- Non-regional TCP and broad forced-proxy overrides use the assigned server source.
- Ordinary IPv4 UDP retains its existing WAN path; QUIC is refused. Public ICMP does not become
  newly forwarded by the TCP optimization. Local access remains available.
- Worker failure, missing tables/TUNs, malformed batches, all-proxy policy, WAN unavailability and
  removal of device-bound routes remain protected. A failed main instance leaves the guest exit
  working. Cleanup preserves unrelated IPv6 table contents.
- DNS works over UDP and TCP, regional answers use the shared policy, and alternative queries
  demonstrably reach DNS through cpxy. AAAA queries receive no addresses. Replacing the router
  binary and reloading restarts DNS/packet workers, keeping embedded data updates aligned.
- Unchanged reload preserves worker PIDs. Actual firewall reload, a netifd gateway change,
  broad proxy-policy update, reboot and explicit stop/removal retain their intended behavior.
- Continuous non-regional IPv4 TCP probes run through reloads, WAN changes, binary replacement
  and reboot. Destination source assertions and a WAN packet capture detect direct escape,
  including packets from unsuccessful connection attempts. Both firewall generations passed
  without direct escape. Explicit stop is tested separately because it intentionally restores WAN.

The tests cover the reasons for the extra safeguards: per-prefix unreachable routes survive WAN
teardown, a firewall bootstrap barrier bridges netifd's initial policy-rule reset, and an open
stdin prevents old iproute2 batch failures under procd event scripts. The barrier has no country
sets; GeoIP classification remains in the routing tables.

## Reproduction and remaining hardware work

Build the router and server with the workspace lockfile, then run:

```sh
cargo test --locked --offline -p client -p mobile-engine -p geoip-data
packaging/openwrt/test/lab.sh /path/to/host-binaries
packaging/openwrt/test/init-dryrun.sh /path/to/cpxy.ipk
packaging/openwrt/test/opkg-roundtrip.sh /path/to/cpxy.ipk
python3 packaging/openwrt/test/apk-package.py
packaging/openwrt/test/apk-roundtrip.sh

# Repeat for each firmware, using its exact kernel/dependency packages.
unshare -Urmn python3 packaging/openwrt/test/qemu-geo.py \
  /path/to/openwrt.img /path/to/cpxy.ipk /path/to/dependency-ipks \
  /path/to/static/server /path/to/results
```

Include `cpxy-router` and `cpxy-server` in the namespace binary directory. VM installation needs
a static x86-64 router binary. The VM harness writes serial, fixture/server and package-download
logs plus `wan.pcap` to its results directory; it exits unsuccessfully on assertion failure.
ShellCheck uses the existing `-s sh -S warning -e SC3043` settings.

Physical GL.iNet/vendor firmware, Wi-Fi, sustained throughput, actual route memory/load cost,
DHCP/PPPoE reconnects and hardware offload remain hardware validation work. Software/hardware
offloading is rejected in GeoIP mode. ECMP/custom PBR integration and seamless migration of
existing connections are outside this delivery. IPv6 is refused, with no IPv6 routing or NAT66
feature added. Keep GeoIP opt-in until the physical-router gate is complete.
