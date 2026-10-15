# cpxy on OpenWrt / GL.iNet: packaging

One package, `cpxy`, built from this repository without the OpenWrt SDK. How to install and
configure it is in the [user guide](../../docs/openwrt.md); this page covers what it contains,
what it changes on the router and how it is built and tested.

| Installed as | Built from | Role, per instance |
|---|---|---|
| `/usr/bin/cpxy-client` | `client` crate, `standalone` | HTTP (and SOCKS5) proxy through the instance's server |
| `/usr/bin/cpxy-router` | `mobile-engine`, the shared packet engine | The networks' TCP from `cpxy<i>` to the server (local region: direct) |
| `/usr/bin/cpxy-dns-split` | `client` crate, `dns_split` | The networks' DNS; overseas servers asked through the instance's server |
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
| Kernel | per routed instance: a persistent TUN, four rule priorities and a routing table; the ad sinkhole route `198.18.0.0/24` while an instance blocks ads | reload, `stop`, `opkg remove` |
| `/tmp/cpxy` | saved resource assignments, the services' settings (root only), DNS caches | `stop`, `opkg remove`, reboot |
| `/etc/cpxy/tsgw/<name>` | each exit node's Tailscale identity, kept across sysupgrade | deleted by hand |

`/etc/config/dhcp` and `/etc/config/network` are never edited. Firewall source zones are found
from the instances' networks; a zone must belong to exactly one network's instance when its DNS is
redirected. Firewall UCI commits normalise that file and drop comments, as LuCI does.

## Build and test

```sh
cargo build --release --locked -p mobile-engine --bin cpxy-router
cargo build --release --locked -p client --bin standalone
cargo build --release --locked -p client --features dns-split --bin dns_split
gateway/build.sh aarch64-unknown-linux-musl bins/cpxy-tsgw
# Copy the Rust binaries into bins/ as cpxy-router, cpxy-client and cpxy-dns-split, then:
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

`lab.sh` needs `cpxy-router` and `cpxy-server` for the host, plus `iproute2`, `nft`, `curl` and
`python3`; the container tests need podman or docker. None of them runs the real fw3/fw4, procd or
dnsmasq, so the redirect, the services and local names are verified on a router.

## Fixed proxy routes

Add `list proxy_route '223.5.5.5/32'` (repeat for each IPv4 CIDR) to an instance
or the `defaults` section in `/etc/config/cpxy`. Reload `cpxy-router` and
`cpxy-tsgw` to apply. Matching TCP destination IPs always use the main proxy,
overriding automatic direct/CN routing. An empty list preserves automatic
routing. The HTTP/SOCKS client already proxies every destination.
