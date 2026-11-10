# cpxy on OpenWrt: user guide

## SOCKS5 kernel router

`cpxy-router` requires a plaintext, unauthenticated SOCKS5 proxy. By default each
instance uses its sibling `cpxy-client` on loopback; its SOCKS5 listener is enabled
automatically. Set `router_socks5_proxy 'IP:port'` (or `'[IPv6]:port'`) to use an
external proxy. For an external-only instance, set `client '0'`;
no cpxy server or credentials are then required.

For IPv4, CN/private TCP forwards directly in the kernel; overseas
TCP enters TUN and uses SOCKS5. `proxy_route` overrides CN/private classification,
except router-local and connected-network access. UDP forwards directly for all
destinations except UDP/443, which is rejected (including QUIC/HTTP/3). TCP/853
follows ordinary TCP splitting. IPv4 ICMP (including ping) uses ordinary WAN
routing directly, regardless of `proxy_route`; it cannot travel through SOCKS5.
Essential local ICMPv6/neighbor discovery remains available. The router's own
outbound sockets use ordinary system routing.

The router and split DNS use the same bundled CN datasets. No extra GeoIP/PBR
package is needed. DNS on UDP/TCP 53 is redirected for both address families to a
dual-stack `[::]:port` listener. Split DNS races Chinese and overseas resolvers,
prefers a regional answer from Chinese resolvers, then uses an overseas answer.
Resolver destinations are classified in userspace: direct where eligible,
otherwise SOCKS5. Proxied UDP DNS specifications are upgraded to TCP. DoT and DoH
work through SOCKS5 too. Existing `?via=cpxy` DNS specifications remain an explicit
proxy override and use the selected SOCKS5 endpoint in the router. Local names
and private reverse lookups still go to dnsmasq directly.

### Migration and routing lifecycle

Stop the old router service before upgrading its routing configuration. Set
`router_geoip '1'` and `dns_listen '[::]:5335'` (port is
5335 + 10 × instance index). Disable firewall `flow_offloading` and
`flow_offloading_hw`, reload the firewall, commit cpxy configuration, then start
`cpxy-client` and `cpxy-router`. Explicit stop restores ordinary WAN access.
Existing legacy `router_geoip '0'` and IPv4-only DNS listeners are rejected with
migration errors. Old `router_ipv6` and `ipv6` settings are ignored; routed network clients
always use IPv4. AAAA
queries receive empty answers locally without contacting either resolver. The
router host and cpxy-client can still use IPv6 to reach upstream servers.

Each instance reserves two IPv4 routing tables (configured table and table + 10000)
and five rule priorities. IPv6 uses an ingress reject rule and no policy tables;
reload removes the old IPv6 tables and forwarding selectors. IPv4 supports one
simple default route, including gateway and point-to-point uplinks. A missing
default keeps regional destinations unreachable until the uplink recovers.
ECMP/custom PBR requires separate integration. Normal WAN forwarding and IPv4 masquerading must already
be configured; the package preserves administrator firewall denies.

Route generations and independent terminal guards prevent overseas TCP falling
back to WAN if tables, TUN, worker or proxy fail. Direct regional traffic remains
independent of worker/proxy health. The firewall boot include blocks forwarding
until IPv4 has capture and terminal rules and IPv6 has its terminal reject rule.
Non-secret resource snapshots under `/etc/cpxy/router-guards/` survive stop for the next enabled boot; uninstall
removes them. Changing TUN/table/priorities requires stop/start. Policy changes may
reconnect affected flows.

There is no engine traffic UI or history API. Physical-router
throughput, route memory costs and vendor firmware still need hardware validation.
The namespace and packaging checks are documented in the
[implementation plan](socks5-kernel-router-plan.md).

The package also provides an HTTP proxy. It retains its existing cpxy configuration and can be
disabled independently.

## Before you start

- A router on **OpenWrt 25.12 or newer** (native apk v3, fw4, procd), **OpenWrt 24.10** (opkg), or GL.iNet's stock 4.x firmware (OpenWrt
  21.02-based, fw3). This revision still needs physical-router validation. For apk, `/etc/apk/arch`
  must list `aarch64`, `aarch64_generic`, `aarch64_cortex-a53`, `aarch64_cortex-a72`,
  `aarch64_cortex-a76` or `x86_64`. On older firmware, `opkg print-architecture` must list
  `aarch64_cortex-a53` or `x86_64`.
- SSH access as `root`.
- For cpxy-client, a running cpxy server per instance (one can serve several), and its URL with the key:
  `https://:<key>@<host>:<port>` (or `http://`).
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
apk info -e ip-full kmod-tun
apk --no-network --repositories-file /dev/null --allow-untrusted add /tmp/cpxy.apk
```

If dependencies are missing, supply matching OpenWrt packages first. In particular, `kmod-tun`
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
opkg update && opkg install /tmp/cpxy.ipk      # also pulls in ip-full and kmod-tun
```

The package also adds the download server's opkg feed and its signing key, so later versions come
with `opkg update && opkg upgrade cpxy`.

If the older `cpxy-router` package is installed, remove it first (`opkg remove cpxy-router`); its
settings do not carry over. A `smartdns` package, if installed, is neither used nor changed.

### Start and configure either package

The services are enabled, but wait for a server. Set one, and everything starts:

```sh
uci set cpxy.defaults.server='https://:<key>@<host>:<port>'
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
	option server 'https://:<key>@home.example:443'
	...

config instance 'main'
	option index '0'
	list network 'lan'

config instance 'guest'
	option index '1'
	list network 'guest'
	option server 'https://:<other-key>@hk.example:443'   # its own server

config instance 'iot'
	option index '2'
	list network 'iot'
```

`cpxy-show` prints what each instance runs with.

The `index` gives each instance its own ports, devices and routing numbers:

| Option | Default for index `i` | main (0) | guest (1) | iot (2) |
|---|---|---|---|---|
| `client_http_listen` | `[::]:5000+i` | 5000 | 5001 | 5002 |
| `client_socks5_listen` (with `client_socks5 1`) | `[::]:1080+i` | 1080 | 1081 | 1082 |
| `dns_listen` (dns_split) | `[::]:5335+10i` | 5335 | 5345 | 5355 |
| `tun` | `cpxy<i>` | cpxy0 | cpxy1 | cpxy2 |
| `routing_table` | `100+i` | 100 | 101 | 102 |
| `rule_priority` (and the next 4) | `9100+10i` | 9100 | 9110 | 9120 |

Set one of these in an instance only if it clashes with something else on the router; these are
never taken from `defaults`.

### Options

| Option | Default | Meaning |
|---|---|---|
| `server` | none; required for client | The cpxy server URL with its key. Kept out of the process list. |
| `enabled` (instance) | `1` | `0` stops the whole instance. |
| `network` (instance, list) | none | OpenWrt networks whose devices use the instance. Without one, the instance runs only its proxy. |
| `client`, `router` | `1` | Which services the instance runs. |
| `client_socks5` | `0` | `1` adds a SOCKS5 proxy; automatically enabled when the router uses the sibling client. |
| `router_socks5_proxy` | sibling client loopback | External plaintext no-auth SOCKS5 endpoint (`IP:port` or `[IPv6]:port`). |
| `router_geoip` | `1` | Required kernel IPv4 split. IPv6 forwarding is always refused; obsolete `router_ipv6` and `ipv6` settings are ignored. |
| `proxy_route` (list) | none | IPv4 or IPv6 CIDRs whose TCP goes through the server even when it would go direct. |
| `dns_upstream` (list) | `223.5.5.5` (AliDNS) | DNS servers whose answer is used, trimmed to its local-region addresses, when it has any. The fastest ones are usually your ISP's. |
| `dns_alternative` (list) | `tcp://1.1.1.1?via=cpxy` | DNS servers whose answer is used otherwise, through the router SOCKS5 endpoint. |
| `dns_search_domain` (list) | empty | Ordered suffixes for single-label names, for router DNS. Tries each suffix before the bare name. |
| `dns_cache` | `1` | dns_split's answer cache, in RAM. |
| `dns_local_domain` (list) | dnsmasq's `domain` and `local` | Domains dns_split leaves to the router (`dns_local_server`), along with single-label names and reverse lookups of private addresses. |
| `dns_local_server` | `127.0.0.1:53` | The router's own resolver, dnsmasq. |
| `router_dns` | `1` | Answer IPv4/IPv6 DNS from the instance's networks with its dns_split. |

A DNS server entry (`dns_upstream`, `dns_alternative`) is a plain IP, or a `udp://`, `tcp://`,
`tls://` or `https://` URL; `tls://` and `https://` accept `?ip=<addr>`. `tcp://`, `tls://` and
`https://` accept the legacy `?via=cpxy` explicit proxy override, through the router's SOCKS5
endpoint. The proxy resolves a hostname without `?ip=` itself, e.g.
`https://dns.google/dns-query?via=cpxy`. Ordinary router specs select their path by IP.

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
queries use SOCKS5, converting plain UDP DNS to TCP. DNS over TLS/HTTPS retains
certificate verification. Hostname resolver endpoints use the system resolver for
bootstrap unless given `?ip=` or an explicit `?via=cpxy` proxy override. The
router's system resolver remains dnsmasq and is never redirected into split DNS.

Because DNS is redirected per firewall zone, a zone holding one instance's networks must not hold
networks of another instance or of none (the service says so). Set `router_dns` to `0` for such an
instance: its devices then use dnsmasq as before.

Names dnsmasq answers specially outside those domains (an `address=/name/` entry, or GL.iNet's
`console.gl-inet.com`) are resolved publicly for redirected networks; add their domains to
`dns_local_domain` to keep dnsmasq's answer.

dns_split runs inside the instance's `cpxy-router`, so while that is down or restarting, its
networks lose proxied TCP and split DNS until procd starts it again; native CN TCP and UDP continue.

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

- **Overseas TCP fails closed.** Worker/proxy failure does not cause WAN fallback.
- **IPv4 UDP is direct**, except UDP/443 is rejected.
- **IPv4 ping is direct**, including when the proxy is down.
- **Router-network IPv6 forwarding is refused**, and AAAA queries return empty answers.
  IPv6 remains available to router-originated connections, including cpxy-client.
- **Only selected networks are captured**, not router-originated connections.
- **Software/hardware flow offloading must be disabled** for kernel routing mode.

## Troubleshooting

| Symptom | Check |
|---|---|
| Nothing starts | `cpxy-show` names what is missing or clashing. |
| A network's devices are not proxied | `logread -e cpxy` for "has no device yet"; check `network` against `uci show network`. |
| A network has no internet | Check the SOCKS5 endpoint with `logread -e cpxy-router`; for the sibling proxy, also check `logread -e cpxy-client`. |
| Names do not resolve | `logread -e cpxy-router`: it shows each DNS server it uses, and the errors reaching them or the SOCKS5 endpoint. |
| Local names (`printer.lan`) do not resolve | `dns_local_domain`, and that dnsmasq answers on `dns_local_server`. |
