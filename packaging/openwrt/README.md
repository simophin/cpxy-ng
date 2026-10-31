# cpxy on OpenWrt / GL.iNet: packaging

One package, `cpxy`, in opkg `.ipk` and native OpenWrt apk v3 formats, built without the OpenWrt SDK. How to install and
configure it is in the [user guide](../../docs/openwrt.md); this page covers what it contains,
what it changes on the router and how it is built and tested.

| Installed as | Built from | Role, per instance |
|---|---|---|
| `/usr/bin/cpxy-client` | `client` crate, `standalone` | HTTP and SOCKS5 proxy through the instance's cpxy server |
| `/usr/bin/cpxy-router` | `mobile-engine`, the shared packet engine | Overseas TCP from `cpxy<i>` through SOCKS5, native CN IPv4/IPv6 routing, and split DNS |
| `/usr/bin/cpxy-tsgw` | `tsgw/` (tailscale-rs, with the shared packet engine) | Tailscale exit node |
| `/usr/bin/cpxy-show` | `files/usr/bin/cpxy-show` | Prints each instance's effective settings |

Each binary is a procd instance named after its cpxy instance, under one init script per program:
`/etc/init.d/cpxy-client`, `cpxy-router` (packet engine and dns_split) and `cpxy-tsgw`. All three
read `/etc/config/cpxy` through `/usr/libexec/cpxy/instance.sh`, which merges an instance with
`defaults`, derives ports and routing numbers from its `index`, and checks every enabled instance
together (indexes, ports, servers, DNS servers). Any of the three refuses a configuration that
fails, leaving what runs untouched. Each subscribes to changes of `cpxy`, and procd restarts only
the instances whose command or environment changed.

```text
LAN TCP (IPv4/IPv6) -> kernel CN/private routes -> WAN
                   -> otherwise TUN -> cpxy-router -> SOCKS5 -> cpxy-client or external proxy
LAN UDP (IPv4/IPv6) -> WAN, except UDP/443 rejected
LAN DNS (UDP/TCP 53, both families) -> redirect -> split DNS
    -> CN/private resolver direct; overseas resolver through SOCKS5 over TCP/DoT/DoH
```

## No loops through the router's own DNS

dnsmasq is not changed, so the router's own lookups (the cpxy servers' names, opkg, NTP) never
reach split DNS, and nothing waits on itself at boot. cpxy-client resolves its server
with dnsmasq. dns_split bootstraps resolver names with the system resolver unless
explicitly proxied, in which case the SOCKS5 endpoint resolves them. It sends
single-label names, `dns_local_domain` and private reverse lookups to dnsmasq, which answers them
itself (`local=/lan/`, DHCP names) and never forwards to dns_split.

## What it changes, and how it is undone

| Where | Change | Removed by |
|---|---|---|
| Package files | binaries, init scripts, helpers, default configuration | `opkg remove` |
| `/etc/config/firewall` | per routed instance: `cpxy_inst_<name>_zone` (its TUN), forwarding from each source zone, UDP refusal, and with `router_dns` IPv4/IPv6 DNS redirects per source zone | the instance's removal on reload, `cpxy-router stop`, `opkg remove` |
| Kernel | per routed instance: a persistent TUN, five rule priorities and two routing tables in both families | reload, `stop`, `opkg remove` |
| `/tmp/cpxy` | saved resource assignments, the services' settings (root only) | `stop`, `opkg remove`, reboot |
| `/etc/cpxy/tsgw/<name>` | each exit node's Tailscale identity, kept across sysupgrade | deleted by hand |

`/etc/config/dhcp` and `/etc/config/network` are never edited. Firewall source zones are found
from the instances' networks; a zone must belong to exactly one network's instance when its DNS is
redirected. Firewall UCI commits normalise that file and drop comments, as LuCI does.

## Build and test

```sh
cross build --release --locked --target aarch64-unknown-linux-musl \
  -p mobile-engine -p client -p cpxy-tsgw --bin cpxy-router --bin standalone --bin cpxy-tsgw
# Copy the binaries into bins/ as cpxy-router, cpxy-client and cpxy-tsgw, then:
packaging/openwrt/build-ipk.sh aarch64_cortex-a53 0.1.0 bins out

# Install, check, remove, and compare the settings before and after, in an OpenWrt 24.10 container
packaging/openwrt/test/opkg-roundtrip.sh out/*.ipk
# The three init scripts with three instances (stubbed procd and routing), and real UCI firewall
packaging/openwrt/test/init-dryrun.sh out/*.ipk
# Routing and fail-closed behaviour with real binaries in network namespaces (no root needed)
packaging/openwrt/test/lab.sh bins
```

CI builds the package for `aarch64_cortex-a53` (GL-MT3000) and `x86_64` on `main`, on tags,
and on demand (`gh workflow run ci.yml --ref <branch>`); pull requests run the packaging tests
with stand-in binaries. Builds of `main` and tags are published as signed opkg feeds
(`make-feed.sh`, tested by `test/feed.sh`); see [docs/downloads.md](../../docs/downloads.md).

### Native apk v3 (OpenWrt 25.12+)

`build-apk.sh` takes the same three binaries as the ipk builder. With apk-tools 3 and
OpenSSL on PATH (and `fakeroot` when not running as root):

```sh
packaging/openwrt/build-apk.sh aarch64_cortex-a53 1.17.0-r10 bins out
# Build every label for these same ARM64 binaries, in a separate directory per label:
packaging/openwrt/build-apk-variants.sh aarch64-unknown-linux-musl 1.17.0-r10 bins out/apk
APK_TOOL=/path/to/apk python3 packaging/openwrt/test/apk-package.py
packaging/openwrt/test/apk-roundtrip.sh
```

Use a label from the router's `/etc/apk/arch`, not `apk --print-arch` (the compiled default).
The supported ARM64 labels are `aarch64`, `aarch64_generic`, `aarch64_cortex-a53`,
`aarch64_cortex-a72` and `aarch64_cortex-a76`; x86-64 uses `x86_64`.
The four OpenWrt-specific ARM64 labels match the [OpenWrt 25.12 package targets](https://downloads.openwrt.org/releases/25.12.0/packages/).
The plain `aarch64` variant remains available for systems accepting that label.
`apk-architectures.sh` is the shared list for builder validation and CI fan-out.

CI compiles once per Rust musl target with no CPU-specific tuning. Each ARM64 apk contains
the same generic static binaries, so the package label does not imply CPU optimisation.
The existing opkg architectures are unchanged. `APK_TOOL` selects
the host apk executable; `CPXY_APK_SIGN_KEY` optionally signs the package. The default
package is unsigned and is verified with the release's SHA256SUMS before local installation.
CI runs the builder in Alpine 3.23 as root, stores the variants in each build artifact's
`apk/<apk arch>/` subdirectories, and publishes every variant separately under
`openwrt-apk/<apk arch>/cpxy.apk`. The existing opkg feed remains under `openwrt/`.

The native package includes OpenWrt's `/lib/apk/packages/cpxy.list`, `.conffiles` and
`.conffiles_static`. Its hooks call OpenWrt's `default_postinst` / `default_prerm` with
`pkgname=cpxy`, using `PKG_UPGRADE=1` for upgrades so disabled services remain disabled.
The apk adds no repository, opkg key, or apk trust key. It declares `ip-full`, `kmod-tun`
and a conflict with the legacy `cpxy-router` package. Use a local `apk add` with networking
disabled when the dependencies are already present.

`apk-package.py` uses temporary roots and stand-in binaries to check metadata, root
ownership, config permissions/preservation, all variant labels and identical payloads,
rejection of a generic apk by a Cortex-A53 arch file, offline installation, revision ordering and
removal with apk-tools itself. `apk-roundtrip.sh` separately runs the hooks in an OpenWrt
25.12 container, including preservation of disabled services and unrelated network/DHCP/
firewall configuration. It needs Docker or Podman. Neither test verifies live procd/fw4
or traffic on physical hardware.

The builder follows [apk-tools' mkpkg format](https://github.com/alpinelinux/apk-tools/blob/v3.0.5/doc/apk-mkpkg.8.scd)
and [OpenWrt's native package metadata/hooks](https://github.com/openwrt/openwrt/blob/main/include/package-pack.mk).

`lab.sh` needs `cpxy-router` and `cpxy-server` for the host, plus `iproute2`, `nft`, `curl` and
`python3`; the container tests need podman or docker. None of them runs the real fw3/fw4, procd or
dnsmasq, so the redirect, the services and local names are verified on a router.

## Fixed proxy routes

Kernel routing is the normal router path: `router_geoip=1`, `router_ipv6=full`.
Both families reserve two table generations and five priorities. The router requires
SOCKS5; by default the sibling cpxy-client listener is enabled automatically. An
external-only instance can disable client/tsgw and omit cpxy server credentials.
See the [operator guide](../../docs/openwrt.md#socks5-kernel-router) for migration.

`test/lab.sh` requires `cpxy-router`, `cpxy-client` (standalone), and `cpxy-server`.
It runs both-family forwarding, proxy chains, overrides and failure guards with real
binaries. `test/init-dryrun.sh` uses stand-ins for procd and the binaries with real
OpenWrt UCI; package install/removal has separate fixtures. Historical validation
reports describe their original revisions; current results live in the
[SOCKS5 plan](../../docs/socks5-kernel-router-plan.md).

Add `list proxy_route '223.5.5.5/32'` (repeat for each IPv4 or IPv6 CIDR) to an instance
or the `defaults` section in `/etc/config/cpxy`. Reload `cpxy-router` and
`cpxy-tsgw` to apply. Matching TCP destination IPs always use the main proxy,
overriding automatic direct/CN routing. An empty list preserves automatic
routing. The HTTP/SOCKS client already proxies every destination.

The separate opt-in [HTTP-controlled RTP router](rtp-router.md) preserves regional
direct routing and split DNS while keeping this original router available.
