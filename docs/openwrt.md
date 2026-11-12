# cpxy on OpenWrt: user guide

## How a routed network is handled

For each instance with networks, the kernel does the routing and `cpxy-client` does the proxying.
`cpxy-router` installs one nftables table, `inet cpxy`, and answers the networks' DNS:

```text
LAN IPv4 TCP to CN/private/connected addresses -> kernel forwarding -> WAN
LAN IPv4 TCP to anything else -> nft redirect -> cpxy-client (transparent) -> cpxy server
LAN IPv4 UDP -> WAN, except UDP/443, which is rejected
LAN IPv4 ICMP -> WAN
LAN IPv6 forwarding -> refused; AAAA queries get empty answers
LAN DNS (UDP/TCP 53, both families) -> redirect -> split DNS in cpxy-router
```

The direct set is the bundled CN dataset plus private ranges, minus `proxy_route`, plus the
routes of the router's main table other than the default (connected and static subnets). TCP
whose route leaves through a device other than an uplink (for example `tailscale0`, or another
LAN) also goes there directly; uplinks are the default route's devices and those of masquerading
firewall zones such as `wan`. Each
instance has its own set, redirect and guard rules, matched by its networks' devices. Traffic to
the router's own addresses and everything the router originates (dnsmasq, cpxy-client itself,
opkg) is never redirected. TCP/853 follows ordinary TCP splitting.

A redirected connection reaches `cpxy-client`'s transparent listener (`client_transparent_listen`,
`0.0.0.0:5100+i`), which reads its original destination from conntrack and opens it through the
instance's cpxy server by IP. The listener refuses connections made to it directly. The package
adds a firewall rule accepting the listener port from each source zone, so zones whose input
policy is REJECT work too; the listener is not reachable from WAN.

Split DNS races Chinese and overseas resolvers, prefers a regional answer from Chinese
resolvers, then uses an overseas answer. Resolver destinations are classified in userspace:
direct where eligible, otherwise through the instance's `cpxy-client` SOCKS5 listener on
loopback. Proxied UDP DNS specifications are upgraded to TCP. `?via=cpxy`
DNS specifications remain an explicit proxy override. Local names and private reverse lookups
still go to dnsmasq directly.

### Failing closed

A forward-hook guard in the same table rejects any forwarded IPv4 TCP from the networks that is
not to a direct address. Redirected connections are delivered to the router, not forwarded, so
they never meet it; anything that missed the redirect is refused rather than sent out the WAN.
With `cpxy-client` down, overseas TCP is refused; direct TCP, UDP and ping keep working.

The table is replaced as a whole in one `nft -f` transaction on every apply: a `proxy_route`
change or an interface event never leaves a partial policy, and established connections keep
their NAT. nftables state does not survive a reboot, so a firewall include
(`/usr/libexec/cpxy/boot-firewall.sh`) rejects all forwarding from the routed devices until the
table guards them. Its device lists are kept in `/etc/cpxy/router-guards/`, which survives an
explicit stop for the next enabled boot; uninstall removes it. An explicit stop restores
ordinary WAN access.

### Requirements and upgrading from the TUN router

The router needs fw4 (OpenWrt 22.03 or newer); the package depends on `firewall4` and
`kmod-nft-nat`, which fw4 firmware already has. No TUN device, policy routing or extra GeoIP
package is used. Disable firewall `flow_offloading` and `flow_offloading_hw`: offloaded flows
bypass the redirect. Normal LAN-to-WAN forwarding and IPv4 masquerading must already be
configured; the package preserves administrator firewall denies.

Upgrading from the TUN-based router needs no manual steps. On its first apply the new table is
installed first, then each instance's old TUN device, routing tables and policy rules are
removed. `tun`, `routing_table` and `rule_priority` are ignored with a log line. Routed TCP now
always goes through the instance's own `cpxy-client`: `router_socks5_proxy` and `client '0'`
on an instance with networks are rejected. `router_geoip` must stay `1`. Old `router_ipv6` and
`ipv6` settings are ignored; routed network clients always use IPv4. The router host and
cpxy-client can still use IPv6 to reach upstream servers.

There is no traffic UI or history API. The namespace lab, the init dry run and a booted
OpenWrt VM test cover this design (see the [plan](archive/redirect-router-plan.md)); physical
routers still need throughput and long-running validation.

The package also provides an HTTP proxy (CONNECT tunnels only). It retains its existing cpxy configuration and can be
disabled independently.

## Before you start

- A router on **OpenWrt 25.12 or newer** (native apk v3, fw4, procd), **OpenWrt 24.10** (opkg), or another fw4 firmware (OpenWrt 22.03 or newer).
  fw3 firmware, such as GL.iNet's stock 4.x (OpenWrt 21.02-based), is not supported. For apk, `/etc/apk/arch`
  must list `aarch64`, `aarch64_generic`, `aarch64_cortex-a53`, `aarch64_cortex-a72`,
  `aarch64_cortex-a76` or `x86_64`. On older firmware, `opkg print-architecture` must list
  `aarch64_cortex-a53` or `x86_64`.
- SSH access as `root`.
- For cpxy-client, a running cpxy server per instance (one can serve several), and its URL with the key:
  `http://:<key>@<host>:<port>`.
- For more than one instance, the networks (and their SSIDs and firewall zones) already set up in
  OpenWrt, each network in a firewall zone of its own.
- About 60 MB of free flash, and about 30 MB of RAM for each instance.

## Install

### OpenWrt 25.12 or newer: direct apk install

Use the native **OpenWrt** apk, not the Android app or the opkg ipk. Read `cat /etc/apk/arch`
on the router and choose a package with an accepted label: for example,
`aarch64_cortex-a53` for the Cudy TR3600, or `x86_64` for x86-64 firmware.
`apk --print-arch` reports apk's compiled default, which can differ from the firmware's
accepted package architecture. Do not change `/etc/apk/arch` to accommodate a download.

All ARM64 variants contain the same generic static ARM64 binaries. The separate package
labels let apk match the firmware; they do not indicate CPU-specific compilation.

The controller can download the package and copy it to the router, so the router needs no
access to the download server or a package repository. Resolve `/latest` once, then download
that version's `openwrt-apk/<arch>/cpxy.apk` and `SHA256SUMS`. Check the SHA-256 entry for that
exact path before copying. For example, on the controller:

```sh
download=https://cpxy-download.cloudflare-087.workers.dev
version=$(curl -fsSL "$download/latest")
arch=aarch64_cortex-a53 # use the value from the router's /etc/apk/arch
curl -fLo cpxy.apk "$download/$version/openwrt-apk/$arch/cpxy.apk"
curl -fLo SHA256SUMS "$download/$version/SHA256SUMS"
awk -v path="openwrt-apk/$arch/cpxy.apk" '$2 == path { print $1 "  cpxy.apk" }' SHA256SUMS | sha256sum -c -
scp -O cpxy.apk root@192.168.2.1:/tmp/
```

On the router, first check that the firmware-specific dependencies are already installed:

```sh
apk info -e firewall4 kmod-nft-nat
apk --no-network --repositories-file /dev/null --allow-untrusted add /tmp/cpxy.apk
```

If dependencies are missing, supply matching OpenWrt packages first. In particular, `kmod-nft-nat`
must match the running kernel; do not substitute a module from another firmware build.
The native apk does not add a repository. It is unsigned by default, hence `--allow-untrusted`
after checking its HTTPS-published checksum. Builds may optionally use `CPXY_APK_SIGN_KEY`;
a trusted signing key then removes the need for `--allow-untrusted`.

Upgrade by repeating the direct download, checksum check, copy and local `apk add` with a
new version. Modified `/etc/config/cpxy` is preserved, and services
disabled by the administrator stay disabled across upgrades. `apk del cpxy` stops/disables
the services and removes their routing/firewall state; modified configuration is retained.

### OpenWrt 24.10 / opkg firmware

Download the package from the [download server](downloads.md), at
`https://<download host>/latest/openwrt/<arch>/cpxy.ipk` (or the `openwrt-<arch>` artifact of a CI
run), copy it to the router and install it:

```sh
curl -fLo cpxy.ipk https://<download host>/latest/openwrt/aarch64_cortex-a53/cpxy.ipk
scp -O cpxy.ipk root@192.168.8.1:/tmp/
ssh root@192.168.8.1
opkg update && opkg install /tmp/cpxy.ipk      # needs firewall4 and kmod-nft-nat
```

The package also adds the download server's opkg feed and its signing key, so later versions come
with `opkg update && opkg upgrade cpxy`.

If the older `cpxy-router` package is installed, remove it first (`opkg remove cpxy-router`); its
settings do not carry over. A `smartdns` package, if installed, is neither used nor changed.

### Start and configure either package

The services are enabled, but wait for a server. Set one, and everything starts:

```sh
uci set cpxy.defaults.server='http://:<key>@<host>:<port>'
uci commit cpxy
```

The `main` instance now covers the `lan` network. Check it with `cpxy-show` and `logread -e cpxy`.

## Configure

Settings live in `/etc/config/cpxy`, which lists every option with its default. Change them with
`uci` and `uci commit cpxy`: each service reloads by itself, restarting only what changed. If the
new settings are not valid, nothing is applied and the services keep running as they were;
`cpxy-show` and `logread -e cpxy` say why.

### Instances and defaults

Every option can be set in the `defaults` section and in an instance; the instance wins. An
instance needs only an `index` (0 to 9, unique) and its networks:

```uci
config defaults 'defaults'
	option server 'http://:<key>@home.example:8080'
	...

config instance 'main'
	option index '0'
	list network 'lan'

config instance 'guest'
	option index '1'
	list network 'guest'
	option server 'http://:<other-key>@hk.example:8080'   # its own server

config instance 'iot'
	option index '2'
	list network 'iot'
```

`cpxy-show` prints what each instance runs with.

The `index` gives each instance its own ports:

| Option | Default for index `i` | main (0) | guest (1) | iot (2) |
|---|---|---|---|---|
| `client_http_listen` | `[::]:5000+i` | 5000 | 5001 | 5002 |
| `client_socks5_listen` (with `client_socks5 1`) | `[::]:1080+i` | 1080 | 1081 | 1082 |
| `client_transparent_listen` (with networks) | `0.0.0.0:5100+i` | 5100 | 5101 | 5102 |
| `dns_listen` (dns_split) | `[::]:5335+10i` | 5335 | 5345 | 5355 |

Set one of these in an instance only if it clashes with something else on the router; these are
never taken from `defaults`.

### Options

| Option | Default | Meaning |
|---|---|---|
| `server` | none; required | The cpxy server URL with its key. Kept out of the process list. |
| `enabled` (instance) | `1` | `0` stops the whole instance. |
| `network` (instance, list) | none | OpenWrt networks whose devices use the instance. Without one, the instance runs only its proxy. |
| `client`, `router` | `1` | Which services the instance runs. An instance with networks needs both. |
| `client_socks5` | `0` | `1` adds a SOCKS5 proxy; always on for an instance with networks, whose DNS worker uses it. |
| `client_transparent_listen` | `0.0.0.0:5100+i` | Where the networks' overseas TCP is redirected. Must bind `0.0.0.0`. |
| `router_geoip` | `1` | Required kernel IPv4 split. IPv6 forwarding is always refused; obsolete `router_ipv6` and `ipv6` settings are ignored. |
| `proxy_route` (list) | none | IPv4 or IPv6 CIDRs whose TCP goes through the server even when it would go direct. |
| `dns_upstream` (list) | `223.5.5.5` (AliDNS) | DNS servers whose answer is used, trimmed to its local-region addresses, when it has any. The fastest ones are usually your ISP's. |
| `dns_alternative` (list) | `tcp://1.1.1.1?via=cpxy` | DNS servers whose answer is used otherwise, through the cpxy server. |
| `dns_search_domain` (list) | empty | Ordered suffixes for single-label names, for router DNS. Tries each suffix before the bare name. |
| `dns_cache` | `1` | dns_split's answer cache, in RAM. |
| `dns_local_domain` (list) | dnsmasq's `domain` and `local` | Domains dns_split leaves to the router (`dns_local_server`), along with single-label names and reverse lookups of private addresses. |
| `dns_local_server` | `127.0.0.1:53` | The router's own resolver, dnsmasq. |
| `router_dns` | `1` | Answer IPv4/IPv6 DNS from the instance's networks with its dns_split. |

A DNS server entry (`dns_upstream`, `dns_alternative`) is a plain IP, or a `udp://` or `tcp://`
URL with an IP address. `tcp://` accepts the `?via=cpxy` explicit proxy override, through the
instance's cpxy-client, e.g. `tcp://8.8.8.8?via=cpxy`. Ordinary router specs select their path by
IP. DNS over TLS (`tls://`) and DNS over HTTPS (`https://`) are no longer supported: an instance
that lists one fails validation and must switch to `tcp://<ip>?via=cpxy`.

**Upgrading from a package with smartdns:** the `smartdns` and `smartdns_*` options are ignored
now. An instance without `dns_alternative` uses `tcp://1.1.1.1?via=cpxy`; one that set
`dns_alternative` to its smartdns (`127.0.0.1:5336`) must change it.

### DNS

With `router_dns` on, UDP/TCP DNS on port 53 from both address families is
redirected to the instance's split resolver. Chinese and overseas resolvers are
queried concurrently. A Chinese response containing regional addresses wins with
nonregional address records removed; otherwise an overseas answer is preferred.
GeoIP classification does not authenticate DNS answers.

Resolver destinations follow regional/forced-proxy routing in userspace. Overseas
queries go through the instance's cpxy-client SOCKS5 listener, converting plain UDP DNS to TCP. The
router's system resolver remains dnsmasq and is never redirected into split DNS.

Because DNS is redirected per firewall zone, a zone holding one instance's networks must not hold
networks of another instance or of none (the service says so). Set `router_dns` to `0` for such an
instance: its devices then use dnsmasq as before, and their TCP is still split by the kernel.

Names dnsmasq answers specially outside those domains (an `address=/name/` entry, or GL.iNet's
`console.gl-inet.com`) are resolved publicly for redirected networks; add their domains to
`dns_local_domain` to keep dnsmasq's answer.

Split DNS runs as `cpxy dns` under the instance's `cpxy-router` service, so while that is down or restarting, its
networks lose split DNS until procd starts it again. While the instance's `cpxy-client` is down,
they lose overseas TCP (refused, never sent direct) and proxied DNS; direct TCP and UDP continue.

## Day-to-day use

- **What runs:** `cpxy-show`, and `logread -e cpxy` for every service's messages.
- **Turn one instance off:** `uci set cpxy.guest.enabled=0; uci commit cpxy`. Its networks go
  direct again; the others keep running.
- **Turn everything off for now:** `for s in cpxy-router cpxy-client; do /etc/init.d/$s stop; done`.
  The networks go direct and use dnsmasq again. `start` (or a reboot) brings them back.
- **Upgrade:** `opkg update && opkg upgrade cpxy`, or install a newer `.ipk` by hand.
  `/etc/config/cpxy` is kept. Upgrading from a package with the Tailscale exit node (cpxy-tsgw)
  removes it; its logins in `/etc/cpxy/tsgw` stay behind until you delete them and remove the
  machines from the Tailscale admin console.
- **Uninstall:** `opkg remove cpxy` stops everything and removes the firewall sections and routing
  it added. A changed `/etc/config/cpxy` and `/etc/cpxy` stay behind; delete them if you do not
  need them.

## Things to know

- **Overseas TCP fails closed.** cpxy-client, server or redirect failure does not cause WAN fallback.
- **IPv4 UDP is direct**, except UDP/443 is rejected.
- **IPv4 ping is direct**, including when the proxy is down.
- **Router-network IPv6 forwarding is refused**, and AAAA queries return empty answers.
  IPv6 remains available to router-originated connections, including cpxy-client.
- **Only selected networks are captured**, not router-originated connections.
- **Software/hardware flow offloading must be disabled**: offloaded flows bypass the redirect.

## Troubleshooting

| Symptom | Check |
|---|---|
| Nothing starts | `cpxy-show` names what is missing or clashing. |
| A network's devices are not proxied | `logread -e cpxy` for "has no device yet"; check `network` against `uci show network`. |
| Overseas sites fail, CN sites work | `logread -e cpxy \| grep -v dns_split`: the client's transparent listener and the cpxy server connection. `nft list chain inet cpxy dstnat` shows the redirect counters. |
| A network has no internet at all | `nft list table inet cpxy_boot_guard`: the boot barrier holds devices the `inet cpxy` table does not guard yet; `logread -e cpxy` says why it is missing. |
| Names do not resolve | `logread -e dns_split`: the DNS worker shows each DNS server it uses, and the errors reaching them or cpxy-client. |
| Local names (`printer.lan`) do not resolve | `dns_local_domain`, and that dnsmasq answers on `dns_local_server`. |
