# cpxy on OpenWrt / GL.iNet

One package, `cpxy-router`, runs three programs on the router and wires them into OpenWrt:

| Program | Installed as | Role |
|---|---|---|
| `client_cn` | `/usr/bin/cpxy-client` | SOCKS5 proxy on `127.0.0.1:1080`; CN destinations go direct, the rest through your cpxy server |
| tun2proxy (built by CI, pinned) | `/usr/bin/cpxy-tun2proxy` | Turns routed LAN TCP into SOCKS5 connections |
| `dns_split` | `/usr/bin/cpxy-dns-split` | DNS resolver for the LAN, behind dnsmasq on `127.0.0.1:5353` |

```text
LAN device ── DNS ──► dnsmasq (DHCP, local names) ──► dns_split
           └─ TCP ──► ip rule: from LAN ► table 100: default dev cpxy0
                      ► tun2proxy ► client_cn (SOCKS5) ► cpxy server  (CN traffic: direct)
```

Targets OpenWrt 24.10 (opkg, fw4/nftables, procd): built for `aarch64_cortex-a53` (GL-MT3000 on
the `-op24` firmware) and `x86_64` (VMs). Check yours with `opkg print-architecture`.

## Install

Download `cpxy-router_<version>_<arch>.ipk` from the release (or the CI artifact), then:

```sh
scp cpxy-router_*.ipk root@192.168.8.1:/tmp/
ssh root@192.168.8.1
opkg update && opkg install /tmp/cpxy-router_*.ipk      # also pulls in ip-full and kmod-tun
uci set cpxy.main.server='https://:<key>@<host>:<port>'
uci set cpxy.main.enabled='1'
uci commit cpxy
/etc/init.d/cpxy restart
```

Check it: `logread -e cpxy`, `ip rule show`, `ip link show cpxy0`.
Change settings with `uci` (see `/etc/config/cpxy`), then `/etc/init.d/cpxy restart`.

## What it changes, and how it is undone

| Where | Change | Removed by |
|---|---|---|
| Package files | binaries, `/etc/init.d/cpxy`, `/usr/libexec/cpxy/`, `/etc/config/cpxy`, an nftables snippet in `/usr/share/nftables.d/chain-pre/forward/` | `opkg remove` |
| `/etc/config/firewall` | zone `cpxy_zone` (device `cpxy0`) and forwarding `cpxy_fwd` (`lan` → `cpxy`) | `opkg remove` (prerm) |
| Kernel | the `cpxy0` TUN device, `ip rule` priorities 9100/9101 (v4 and v6) and routing table 100 | `stop`, `opkg remove` |
| dnsmasq | a drop-in (`no-resolv`, `server=127.0.0.1#5353`) in dnsmasq's runtime config directory under `/tmp` | `stop`, `opkg remove`, reboot |

`/etc/config/dhcp`, `/etc/config/network` and the existing `lan → wan` forwarding are never edited.
While the service stops, the LAN goes direct again, as before the install. `uci commit` rewrites
`/etc/config/firewall` in normalised form (comments are dropped, settings are unchanged); that
happens on any firewall edit, including LuCI's.

## Behaviour to know about

- **Fails closed while enabled.** The `cpxy0` device outlives tun2proxy and drops what it is sent,
  and table 100 also holds an `unreachable default`. If tun2proxy or client_cn dies, LAN TCP stops
  working (it is never sent out the WAN) until procd restarts them. `stop` deliberately restores
  direct access.
- **TCP only.** UDP from the LAN into the tunnel is rejected immediately (so QUIC falls back to TCP).
  IPv6 from the LAN is refused with the same mechanism, so clients use IPv4. LAN DNS queries to the
  router work normally.
- **Only LAN clients** (the interfaces in `lan_interface`) are proxied. Traffic from the router
  itself is not.
- **DNS.** dnsmasq keeps DHCP and local names; public lookups go to dns_split, which prefers the
  `dns_upstream` answer when all addresses are in the CN region and otherwise uses `dns_alternative`.
  If `dhcp.@dnsmasq[0].server` is set, dnsmasq may still use those servers; the service logs a warning.
- **Flow offloading.** Software offload does not touch the TUN path. Hardware offload (a GL.iNet
  option) may; the service logs a warning when it is on.

## Build and test

```sh
# CI does this per architecture; locally, with the three binaries for your target in bins/
packaging/openwrt/build-ipk.sh aarch64_cortex-a53 0.1.0 bins out

# Install, check, remove, and compare the settings before and after, in an OpenWrt 24.10 container
packaging/openwrt/test/opkg-roundtrip.sh out/*.ipk

# Routing and fail-closed behaviour with real binaries in network namespaces (no root needed)
packaging/openwrt/test/lab.sh bins
```

`lab.sh` needs the binaries named `cpxy-client`, `cpxy-server` (the cpxy server) and `cpxy-tun2proxy`
for the host architecture, plus `iproute2`, `nft`, `curl` and `python3`. `opkg-roundtrip.sh` needs
podman or docker. Neither runs the real fw4, dnsmasq or procd, so the init script, the dnsmasq
drop-in and fw4's handling of the zone and snippet are only verified on a router.
