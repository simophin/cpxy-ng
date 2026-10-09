# cpxy on OpenWrt / GL.iNet

One package, `cpxy-router`, runs two programs built from this repository:

| Program | Installed as | Role |
|---|---|---|
| Native shared packet engine | `/usr/bin/cpxy-router` | Reads LAN TCP from `cpxy0`; private/local-region destinations go direct, others through cpxy |
| `dns_split` | `/usr/bin/cpxy-dns-split` | DNS resolver for the LAN, behind dnsmasq on `127.0.0.1:5335` |

```text
LAN device ── DNS ──► dnsmasq (DHCP, local names) ──► dns_split
           └─ TCP ──► LAN policy routing ► cpxy0 ► shared Rust packet engine
                      ► cpxy server (local-region/private traffic: direct)
           └─ UDP 443 (QUIC) ► cpxy0 ► rejected, browser falls back to TCP
           └─ other UDP ──► WAN, direct
```

The packet engine is shared with the Android VPN in `mobile-engine`. No external tun2proxy
binary, local SOCKS server, or SOCKS proxy configuration is needed. DNS remains separately
supervised to preserve dnsmasq's readiness-based handover and SQLite cache lifecycle.
The existing Rust TCP/IP stack dependency (`ipstack`) is still required at build time.

When upgrading, existing `socks5_listen`, `api_listen` and `dns_server` UCI options are ignored;
the old statistics listener is removed. Unlike the previous `client_cn`, the shared engine does not
send all of `100.0.0.0/8` direct: these addresses follow normal regional routing.
TCP 853 (DNS over TLS) is refused by the shared filter; other UDP retains the router's direct path.

Targets OpenWrt 24.10 (opkg, fw4/nftables, procd) and GL.iNet's stock 4.x firmware (OpenWrt
21.02-based, fw3/iptables): built for `aarch64_cortex-a53` (GL-MT3000) and `x86_64` (VMs). Check
yours with `opkg print-architecture`.

## Install

Download `cpxy-router_<version>_<arch>.ipk` from the release (or the CI artifact), then:

```sh
scp cpxy-router_*.ipk root@192.168.8.1:/tmp/
ssh root@192.168.8.1
opkg update && opkg install /tmp/cpxy-router_*.ipk      # also pulls in ip-full and kmod-tun
uci set cpxy.main.server='https://:<key>@<host>:<port>'
uci add_list cpxy.main.dns_upstream='<server>'      # dns_split: preferred when its answer is all CN
uci add_list cpxy.main.dns_alternative='<server>'   # dns_split: used otherwise
uci set cpxy.main.enabled='1'
uci commit cpxy
/etc/init.d/cpxy restart
```

`server`, `dns_upstream` and `dns_alternative` have no defaults and are required (the last two only
while `dns_split` is `1`): the service logs which one is missing and does not start. A DNS server is
an IP, or a `udp://`, `tcp://`, `tls://` or `https://` URL (`tls://` and `https://` accept
`?ip=<addr>` to skip the startup hostname lookup).

Check it: `logread -e cpxy`, `ip rule show`, `ip link show cpxy0`.
Change settings with `uci` (see `/etc/config/cpxy`), then `/etc/init.d/cpxy restart`.

## What it changes, and how it is undone

| Where | Change | Removed by |
|---|---|---|
| Package files | binaries, `/etc/init.d/cpxy`, `/usr/libexec/cpxy/`, `/etc/config/cpxy`, an nftables snippet in `/usr/share/nftables.d/chain-pre/forward/` | `opkg remove` |
| `/etc/config/firewall` | zone `cpxy_zone` (device `cpxy0`) and forwarding `cpxy_fwd` (`lan` → `cpxy`); on fw3 only, include `cpxy_udp` (`/usr/libexec/cpxy/fw3-include.sh`) | `opkg remove` (prerm) |
| iptables (fw3 only) | `FORWARD -o cpxy0 -p udp -j REJECT`, added by the include on every firewall start and reload | `opkg remove` (prerm) |
| Kernel | the `cpxy0` TUN device, `ip rule` priorities 9100 and 9103 (v4 and v6) and 9101/9102 (UDP, v4), and routing table 100 | `stop`, `opkg remove` |
| dnsmasq | a drop-in (`no-resolv`, `server=127.0.0.1#5335`) in dnsmasq's runtime config directory under `/tmp`, present only while dns_split listens | dns_split exiting, `stop`, `opkg remove`, reboot |

`/etc/config/dhcp`, `/etc/config/network` and the existing `lan → wan` forwarding are never edited.
While the service stops, the LAN goes direct again, as before the install. `uci commit` rewrites
`/etc/config/firewall` in normalised form (comments are dropped, settings are unchanged); that
happens on any firewall edit, including LuCI's.

## Behaviour to know about

- **Fails closed while enabled.** The `cpxy0` device outlives the packet worker and drops what it is sent,
  and table 100 also holds an `unreachable default`. If the packet worker dies, LAN TCP stops
  working (it is never sent out the WAN) until procd restarts it. `stop` deliberately restores
  direct access.
- **TCP is proxied; UDP is not.** cpxy has no UDP path. QUIC (UDP 443) is routed into the tunnel
  and rejected immediately, so browsers fall back to TCP through the proxy rather than reaching sites
  from the WAN address. All other UDP (WebRTC and video calls, games, VoIP, DNS to outside
  resolvers) goes out the WAN directly, so those peers see the router's real address. IPv6 from the
  LAN is refused, so clients use IPv4. LAN DNS queries to the router work normally.
- **Only LAN clients** (the interfaces in `lan_interface`) are proxied. Traffic from the router
  itself is not.
- **DNS.** dnsmasq keeps DHCP and local names; public lookups go to dns_split, which prefers the
  `dns_upstream` answer when all addresses are in the CN region and otherwise uses `dns_alternative`.
  If `dhcp.@dnsmasq[0].server` is set, dnsmasq may still use those servers; the service logs a warning.
  The dns instance runs dns_split under `/usr/libexec/cpxy/dns-split.sh`, which adds the dnsmasq
  drop-in once dns_split listens on UDP and TCP and removes it when dns_split exits. Unlike TCP,
  DNS fails open: while dns_split is down, dnsmasq uses its usual servers again.
- **Firewall backends.** fw4 includes the nftables snippet itself. fw3 does not read it, so on fw3
  the package registers an iptables include doing the same. The zone and forwarding work on both.
- **Flow offloading.** Software offload does not touch the TUN path. Hardware offload (a GL.iNet
  option) may; the service logs a warning when it is on.

## Build and test

```sh
cargo build --release --locked -p mobile-engine --bin cpxy-router
cargo build --release --locked -p client --features dns-split --bin dns_split
# Copy them into bins/ as cpxy-router and cpxy-dns-split.

# CI does this per architecture; locally, with the two binaries for your target in bins/
packaging/openwrt/build-ipk.sh aarch64_cortex-a53 0.1.0 bins out

# Install, check, remove, and compare the settings before and after, in an OpenWrt 24.10 container
packaging/openwrt/test/opkg-roundtrip.sh out/*.ipk

# The init script's commands (stubbed procd), and the dnsmasq hand-over around dns_split
packaging/openwrt/test/init-dryrun.sh out/*.ipk
packaging/openwrt/test/dns-handover.sh out/*.ipk

# Routing and fail-closed behaviour with real binaries in network namespaces (no root needed)
packaging/openwrt/test/lab.sh bins
```

`lab.sh` needs the binaries named `cpxy-router` and `cpxy-server` (the cpxy server)
for the host architecture, plus `iproute2`, `nft`, `curl` and `python3`. `opkg-roundtrip.sh` needs
podman or docker. None of them runs the real fw3/fw4, dnsmasq or procd, so the init script, the
dnsmasq drop-in and the firewall's handling of the zone, snippet and include are only verified on a
router.

The native client was also exercised on a GL-MT3000 with GL.iNet firmware and fw3.
See [hardware validation results](../../docs/openwrt-native-validation.md) for the setup,
checks, measured memory use and remaining hardware coverage.
