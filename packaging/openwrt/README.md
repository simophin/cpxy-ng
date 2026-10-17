# cpxy on OpenWrt / GL.iNet: packaging

One package, `cpxy`, in opkg `.ipk` and native OpenWrt apk v3 formats, built without the OpenWrt SDK. How to install and
configure it is in the [user guide](../../docs/openwrt.md); this page covers what it contains,
what it changes on the router and how it is built and tested.

| Installed as | Built from | Role, per instance |
|---|---|---|
| `/usr/bin/cpxy-client` | `client` crate, `standalone` | HTTP (and SOCKS5) proxy through the instance's server |
| `/usr/bin/cpxy-router` | `mobile-engine`, the shared packet engine | The networks' TCP from `cpxy<i>` to the server (local region: direct), and their DNS with dns_split (overseas servers asked through the server) |
| `/usr/bin/cpxy-tsgw` | `gateway/` (Go, linked to the engine) | Tailscale exit node |
| `/usr/bin/cpxy-show` | `files/usr/bin/cpxy-show` | Prints each instance's effective settings |

Each binary is a procd instance named after its cpxy instance, under one init script per program:
`/etc/init.d/cpxy-client`, `cpxy-router` (packet engine and dns_split) and `cpxy-tsgw`. All three
read `/etc/config/cpxy` through `/usr/libexec/cpxy/instance.sh`, which merges an instance with
`defaults`, derives ports and routing numbers from its `index`, and checks every enabled instance
together (indexes, ports, servers, DNS servers). Any of the three refuses a configuration that
fails, leaving what runs untouched. Each subscribes to changes of `cpxy`, and procd restarts only
the instances whose command or environment changed.

```text
device ── DNS (IPv4, any server) ─► firewall redirect ─► dns_split ─┬─ dns_upstream
       │                                                            ├─ dns_alternative ─► server
       │                                                            └─ local names ─► dnsmasq
       └─ TCP ─► policy routing ─► cpxy<i> ─► packet engine ─► server (local region: direct)
       └─ UDP 443 (QUIC) ─► cpxy<i> ─► rejected; other UDP ─► WAN
```

## No loops through the router's own DNS

dnsmasq is not changed, so the router's own lookups (the cpxy servers' names, opkg, NTP) never
reach cpxy, and nothing waits on itself at boot. dns_split looks up the cpxy server's name with
dnsmasq, and the names of its `?via=cpxy` servers not at all: the cpxy server does. It sends
single-label names, `dns_local_domain` and private reverse lookups to dnsmasq, which answers them
itself (`local=/lan/`, DHCP names) and never forwards to dns_split.

## What it changes, and how it is undone

| Where | Change | Removed by |
|---|---|---|
| Package files | binaries, init scripts, helpers, default configuration | `opkg remove` |
| `/etc/config/firewall` | per routed instance: `cpxy_inst_<name>_zone` (its TUN), forwarding from each source zone, UDP refusal, and with `router_dns` a DNS redirect and an IPv6 DNS refusal per source zone | the instance's removal on reload, `cpxy-router stop`, `opkg remove` |
| Kernel | per routed instance: a persistent TUN, four rule priorities and a routing table | reload, `stop`, `opkg remove` |
| `/tmp/cpxy` | saved resource assignments, the services' settings (root only) | `stop`, `opkg remove`, reboot |
| `/etc/cpxy/tsgw/<name>` | each exit node's Tailscale identity, kept across sysupgrade | deleted by hand |

`/etc/config/dhcp` and `/etc/config/network` are never edited. Firewall source zones are found
from the instances' networks; a zone must belong to exactly one network's instance when its DNS is
redirected. Firewall UCI commits normalise that file and drop comments, as LuCI does.

## Build and test

```sh
cargo build --release --locked -p mobile-engine --bin cpxy-router
cargo build --release --locked -p client --bin standalone
gateway/build.sh aarch64-unknown-linux-musl bins/cpxy-tsgw
# Copy the Rust binaries into bins/ as cpxy-router and cpxy-client, then:
packaging/openwrt/build-ipk.sh aarch64_cortex-a53 0.1.0 bins out

# Install, check, remove, and compare the settings before and after, in an OpenWrt 24.10 container
packaging/openwrt/test/opkg-roundtrip.sh out/*.ipk
# The four init scripts with three instances (stubbed procd and routing), and real UCI firewall
packaging/openwrt/test/init-dryrun.sh out/*.ipk
# Routing and fail-closed behaviour with real binaries in network namespaces (no root needed)
packaging/openwrt/test/lab.sh bins
```

CI builds the package for `aarch64_cortex-a53` (GL-MT3000) and `x86_64` on `main`, on tags,
and on demand (`gh workflow run ci.yml --ref <branch>`); pull requests run the packaging tests
with stand-in binaries. Builds of `main` and tags are published as signed opkg feeds
(`make-feed.sh`, tested by `test/feed.sh`); see [docs/downloads.md](../../docs/downloads.md).

### Native apk v3 (OpenWrt 25.12+)

`build-apk.sh` takes the same four binaries as the ipk builder. With apk-tools 3 and
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

Add `list proxy_route '223.5.5.5/32'` (repeat for each IPv4 CIDR) to an instance
or the `defaults` section in `/etc/config/cpxy`. Reload `cpxy-router` and
`cpxy-tsgw` to apply. Matching TCP destination IPs always use the main proxy,
overriding automatic direct/CN routing. An empty list preserves automatic
routing. The HTTP/SOCKS client already proxies every destination.
