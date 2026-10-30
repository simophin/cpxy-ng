# cpxy on OpenWrt: user guide

## SOCKS5 kernel router

`cpxy-router` requires a plaintext, unauthenticated SOCKS5 proxy. By default each
instance uses its sibling `cpxy-client` on loopback; its SOCKS5 listener is enabled
automatically. Set `router_socks5_proxy 'IP:port'` (or `'[IPv6]:port'`) to use an
external proxy. For an external-only instance, set `client '0'` and `tsgw '0'`;
no cpxy server or credentials are then required.

For both IPv4 and IPv6, CN/private TCP forwards directly in the kernel; overseas
TCP enters TUN and uses SOCKS5. `proxy_route` overrides CN/private classification,
except router-local and connected-network access. UDP forwards directly for all
destinations except UDP/443, which is rejected (including QUIC/HTTP/3). TCP/853
follows ordinary TCP splitting. Essential local ICMPv6/neighbor discovery remains
available. The router's own outbound sockets use ordinary system routing.

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
`router_geoip '1'`, `router_ipv6 'full'` and `dns_listen '[::]:5335'` (port is
5335 + 10 × instance index). Disable firewall `flow_offloading` and
`flow_offloading_hw`, reload the firewall, commit cpxy configuration, then start
`cpxy-client` and `cpxy-router`. Explicit stop restores ordinary WAN access.
Existing legacy `router_geoip '0'`, IPv4-only DNS listeners and `router_ipv6 'off'`
are rejected with migration errors. The shared `ipv6` setting continues to control
Tailscale; the router uses the same policy for both families.

Each instance reserves two routing tables (configured table and table + 10000)
in both families, plus five rule priorities. It supports one simple default route
per family, including gateway and point-to-point uplinks. A missing default keeps
regional destinations unreachable until that family recovers; overseas TCP can
still use a proxy reachable through the other family. ECMP/custom PBR requires
separate integration. Normal WAN forwarding and IPv4 masquerading must already
be configured; the package preserves administrator firewall denies.

Route generations and independent terminal guards prevent overseas TCP falling
back to WAN if tables, TUN, worker or proxy fail. Direct regional traffic remains
independent of worker/proxy health. The firewall boot include blocks forwarding
until both families have capture and terminal rules. Non-secret resource snapshots
under `/etc/cpxy/router-guards/` survive stop for the next enabled boot; uninstall
removes them. Changing TUN/table/priorities requires stop/start. Policy changes may
reconnect affected flows.

Kernel-direct traffic does not appear in the engine traffic API. Physical-router
throughput, route memory costs and vendor firmware still need hardware validation.
The namespace and packaging checks are documented in the
[implementation plan](socks5-kernel-router-plan.md).

The package also provides an HTTP proxy and a Tailscale exit node. Those components
retain their existing cpxy configuration and can be disabled independently.

## Before you start

- A router on **OpenWrt 25.12 or newer** (native apk v3, fw4, procd), **OpenWrt 24.10** (opkg), or GL.iNet's stock 4.x firmware (OpenWrt
  21.02-based, fw3). This revision still needs physical-router validation. For apk, `/etc/apk/arch`
  must list `aarch64`, `aarch64_generic`, `aarch64_cortex-a53`, `aarch64_cortex-a72`,
  `aarch64_cortex-a76` or `x86_64`. On older firmware, `opkg print-architecture` must list
  `aarch64_cortex-a53` or `x86_64`.
- SSH access as `root`.
- For cpxy-client/tsgw, a running cpxy server per instance (one can serve several), and its URL with the key:
  `https://:<key>@<host>:<port>` (or `http://`).
- For more than one instance, the networks (and their SSIDs and firewall zones) already set up in
  OpenWrt, each network in a firewall zone of its own.
- About 60 MB of free flash, and RAM for each instance: about 30 MB, plus about 35 MB for its
  Tailscale exit node. A GL-MT3000 (512 MB) runs three instances with exit nodes.

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
new version. Modified `/etc/config/cpxy` and Tailscale identities are preserved, and services
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
Log the Tailscale exit node in with the link `logread -e cpxy-tsgw` shows (see
[Tailscale](#tailscale)), or turn it off with `uci set cpxy.defaults.tsgw=0`.

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
	option tsgw '0'                                      # no exit node

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
| `router_api_listen` (with `router_api 1`) | `127.0.0.1:3010+i` | 3010 | 3011 | 3012 |
| `tun` | `cpxy<i>` | cpxy0 | cpxy1 | cpxy2 |
| `routing_table` | `100+i` | 100 | 101 | 102 |
| `rule_priority` (and the next 4) | `9100+10i` | 9100 | 9110 | 9120 |
| `tsgw_port` (UDP; not 41641, which `tailscaled` uses) | `41700+i` | 41700 | 41701 | 41702 |
| `tsgw_hostname` | `cpxy-<name>` | cpxy-main | cpxy-guest | cpxy-iot |
| `tsgw_state_dir` | `/etc/cpxy/tsgw/<name>` | | | |

Set one of these in an instance only if it clashes with something else on the router; these are
never taken from `defaults`.

### Options

| Option | Default | Meaning |
|---|---|---|
| `server` | none; required for client/tsgw | The cpxy server URL with its key. Kept out of the process list. |
| `enabled` (instance) | `1` | `0` stops the whole instance. |
| `network` (instance, list) | none | OpenWrt networks whose devices use the instance. Without one, the instance runs only its proxy and exit node. |
| `client`, `router`, `tsgw` | `1` | Which services the instance runs. |
| `client_socks5` | `0` | `1` adds a SOCKS5 proxy; automatically enabled when the router uses the sibling client. |
| `router_socks5_proxy` | sibling client loopback | External plaintext no-auth SOCKS5 endpoint (`IP:port` or `[IPv6]:port`). |
| `router_geoip` / `router_ipv6` | `1` / `full` | Required kernel split for both families. |
| `ipv6` | `full` | IPv6 for mobile/Tailscale clients; router always splits both families. `full` routes it like IPv4, which needs a server that can reach IPv6; `direct` only what goes direct (local-region and private addresses), for a server that cannot; `off` refuses it, so devices use IPv4. The router's WAN needs IPv6, and its networks IPv6 addresses (OpenWrt hands them out by default). |
| `proxy_route` (list) | none | IPv4 or IPv6 CIDRs whose TCP goes through the server even when it would go direct. |
| `dns_upstream` (list) | `223.5.5.5` (AliDNS) | DNS servers whose answer is used, trimmed to its local-region addresses, when it has any. The fastest ones are usually your ISP's. |
| `dns_alternative` (list) | `tcp://1.1.1.1?via=cpxy` | DNS servers whose answer is used otherwise, through the router SOCKS5 endpoint (cpxy for tsgw). |
| `dns_search_domain` (list) | empty | Ordered suffixes for single-label names, shared by router DNS and Tailscale gateway DNS. Tries each suffix before the bare name. |
| `dns_cache` | `1` | dns_split's answer cache, in RAM. |
| `dns_local_domain` (list) | dnsmasq's `domain` and `local` | Domains dns_split leaves to the router (`dns_local_server`), along with single-label names and reverse lookups of private addresses. |
| `dns_local_server` | `127.0.0.1:53` | The router's own resolver, dnsmasq. |
| `router_dns` | `1` | Answer IPv4/IPv6 DNS from the instance's networks with its dns_split. |
| `router_api` | `0` | Enable the offline traffic page and read-only API, on `router_api_listen`. No authentication; loopback by default. See [traffic monitoring](router-traffic.md). |
| `tsgw_authkey` | none | A Tailscale auth key, to log in without the link. |
| `tsgw_control_url` | Tailscale's | A different coordination server, e.g. Headscale. Its certificate must be from a public CA, such as Let's Encrypt: cpxy-tsgw trusts the roots built into it, not the router's. |
| `tsgw_mtu` | `1280` | The largest packet sent back to Tailscale clients. |
| `tsgw_proxy` | `1` | Reach Tailscale's coordination and relay (DERP) servers through the instance's proxy. |
| `tsgw_verbose` | `0` | More logging. |

A DNS server entry (`dns_upstream`, `dns_alternative`) is a plain IP, or a `udp://`, `tcp://`,
`tls://` or `https://` URL; `tls://` and `https://` accept `?ip=<addr>`. `tcp://`, `tls://` and
`https://` accept `?via=cpxy` to ask through the instance's server, which then looks up a host
name without `?ip=` itself, e.g. `https://dns.google/dns-query?via=cpxy`.

**Upgrading from a package with smartdns:** the `smartdns` and `smartdns_*` options are ignored
now. An instance without `dns_alternative` asks the same servers through its server; one that set
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

### Tailscale

Each instance with `tsgw` on is its own machine on your tailnet, `cpxy-<name>`. To set one up:

1. `logread -e cpxy-tsgw` shows `Log in by opening https://login.tailscale.com/a/...`; open it.
2. In the [admin console](https://login.tailscale.com/admin/machines), approve it as an exit node
   (**Edit route settings**, **Use as exit node**) and disable its key expiry.

The login is kept in `/etc/cpxy/tsgw/<name>`, across reboots and firmware upgrades. Phones then
pick `cpxy-<name>` as exit node in the Tailscale app. [The cpxy-tsgw guide](cpxy-tsgw.md) has
the details.

### Traffic view

Enable the embedded browser page for an instance with `uci set cpxy.main.router_api=1`,
`uci commit cpxy`, then `/etc/init.d/cpxy-router reload`. From your computer, run
`ssh -N -L 3010:127.0.0.1:3010 root@192.168.2.1` and open **http://127.0.0.1:3010/**.
Each instance has its own port (`3010+index`) and history. The page works entirely offline,
showing recent TCP connections and tunnel byte totals. It has no authentication; keep the
listener on loopback or a trusted management interface.

See the [traffic API and browser guide](router-traffic.md) for direct LAN access, pagination,
and what traffic the worker records.

## Day-to-day use

- **What runs:** `cpxy-show`, and `logread -e cpxy` for every service's messages.
- **Turn one instance off:** `uci set cpxy.guest.enabled=0; uci commit cpxy`. Its networks go
  direct again; the others keep running.
- **Turn everything off for now:** `for s in cpxy-tsgw cpxy-router cpxy-client; do /etc/init.d/$s stop; done`.
  The networks go direct and use dnsmasq again. `start` (or a reboot) brings them back.
- **Upgrade:** `opkg update && opkg upgrade cpxy`, or install a newer `.ipk` by hand.
  `/etc/config/cpxy` and the Tailscale logins are kept.
- **Uninstall:** `opkg remove cpxy` stops everything and removes the firewall sections and routing
  it added. A changed `/etc/config/cpxy` and `/etc/cpxy` stay behind; delete them if you do not
  need them, and remove the machines from the Tailscale admin console.

## Things to know

- **Overseas TCP fails closed.** Worker/proxy failure does not cause WAN fallback.
- **UDP is direct**, except UDP/443 is rejected for both families.
- **IPv6 follows the same kernel split as IPv4.** Overseas IPv6 TCP needs a proxy
  with IPv6 destination connectivity; CN IPv6 uses the router's native IPv6 uplink.
- **Only selected networks are captured**, not router-originated connections.
- **Software/hardware flow offloading must be disabled** for kernel routing mode.

## Troubleshooting

| Symptom | Check |
|---|---|
| Nothing starts | `cpxy-show` names what is missing or clashing. |
| A network's devices are not proxied | `logread -e cpxy` for "has no device yet"; check `network` against `uci show network`. |
| A network has no internet | The server URL or key is wrong, or the server is unreachable: `logread -e cpxy-router`. |
| Names do not resolve | `logread -e cpxy-router`: it shows each DNS server it uses, and the errors reaching them or the instance's server. |
| Local names (`printer.lan`) do not resolve | `dns_local_domain`, and that dnsmasq answers on `dns_local_server`. |
| The exit node does not appear | It is not logged in or not approved: `logread -e cpxy-tsgw`. |
