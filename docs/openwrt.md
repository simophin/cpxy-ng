# cpxy on OpenWrt: user guide

## Optional kernel IPv4 GeoIP routing

Set `option router_geoip '1'` in an instance or `defaults` to forward regional/private IPv4 TCP
directly in the kernel. The package uses its bundled DB-IP data, shared with split DNS; it needs
no additional GeoIP/PBR package or list downloads. Other captured TCP goes through the instance's
cpxy server. `proxy_route` still overrides regional/private classification, after existing
connected-network exceptions.

This mode is IPv4-only. Leave `router_ipv6 'off'` (its default): forwarded IPv6 is refused to
prevent bypass, and this router's split DNS returns no AAAA answers. Router-local and on-link
IPv6 communication remains possible. The shared `ipv6` option continues to control Tailscale;
it does not enable IPv6 in kernel GeoIP mode. IPv6 `proxy_route` entries are rejected in this mode.

Before first enabling it, stop `cpxy-router`; disable `flow_offloading` and `flow_offloading_hw`
in the firewall defaults and reload the firewall. Set `router_geoip '1'`, commit cpxy and start
the router service. Changing between legacy and kernel GeoIP mode, or changing a running GeoIP
instance's TUN/table/priorities, requires an explicit stop/start. Stop restores ordinary WAN
access, so use a maintenance window if continuous protection is required.

The initial implementation supports one simple IPv4 default in `main`, including gateway-based
and point-to-point uplinks. ECMP and third-party policy-routing configurations need separate
integration. Each GeoIP instance reserves its configured routing table and that table plus
10,000, and five priorities beginning at `rule_priority`. A changed WAN gateway triggers route
reconciliation. Missing policy tables/TUNs refuse proxy traffic; failed batches retain the old
generation or protection. Reloads can reconnect flows whose policy changes.

Existing source-zone forwarding rules must permit direct WAN traffic, with normal WAN IPv4
masquerading. The package preserves administrator firewall denies. Kernel-direct flows no longer
appear in the packet engine's traffic log/API. `cpxy-show` reports the active table, route count
and dataset identity. GeoIP mode remains opt-in pending physical-router validation.

The [implementation and test plan](openwrt-geoip-routing-plan.md) covers failure handling and
the distinction between this TCP router and the future full IP VPN.

The `cpxy` package puts your networks behind cpxy servers, with nothing to set up on the devices.
For each set of networks (for example `lan`, `guest` and `iot`) it runs one **instance**, with its
own cpxy server and three services:

| Service | What it does |
|---|---|
| `cpxy-client` | An HTTP proxy (`[::]:5000` for the first instance), through the instance's server. Devices may use it directly; tsgw uses it too. |
| `cpxy-router` | Sends the networks' TCP through the server (local-region destinations go direct) and answers their DNS with **dns_split**, which picks the local-region answer when there is one, and otherwise the answer of an overseas DNS server (1.1.1.1 over TCP by default) asked through the server, so it is not tampered with. |
| `cpxy-tsgw` | A [Tailscale exit node](cpxy-tsgw.md) with the same routing and split DNS, for phones and laptops away from home. |

```text
device on 'guest' ── DNS ─► dns_split (guest) ─┬─ local-region DNS servers
                                               └─ overseas DNS servers, through guest's server
                  └─ TCP ─► cpxy1 ─► packet engine ─► guest's server (local region: direct)
phone (Tailscale) ───────► cpxy-tsgw (guest) ─► the same, through guest's server
```

The router's own DNS (dnsmasq) is not changed: it keeps serving DHCP and the router itself, and
answers local names (`.lan`, DHCP host names) for dns_split.

## Before you start

- A router on **OpenWrt 25.12 or newer** (native apk v3, fw4, procd), **OpenWrt 24.10** (opkg), or GL.iNet's stock 4.x firmware (OpenWrt
  21.02-based, fw3). Tested on a GL-MT3000 with the stock firmware. For apk, `/etc/apk/arch`
  must list `aarch64`, `aarch64_generic`, `aarch64_cortex-a53`, `aarch64_cortex-a72`,
  `aarch64_cortex-a76` or `x86_64`. On older firmware, `opkg print-architecture` must list
  `aarch64_cortex-a53` or `x86_64`.
- SSH access as `root`.
- A running cpxy server per instance (one can serve several), and its URL with the key:
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
| `dns_listen` (dns_split) | `0.0.0.0:5335+10i` | 5335 | 5345 | 5355 |
| `router_api_listen` (with `router_api 1`) | `127.0.0.1:3010+i` | 3010 | 3011 | 3012 |
| `tun` | `cpxy<i>` | cpxy0 | cpxy1 | cpxy2 |
| `routing_table` | `100+i` | 100 | 101 | 102 |
| `rule_priority` (and the next 3) | `9100+10i` | 9100 | 9110 | 9120 |
| `tsgw_port` (UDP; not 41641, which `tailscaled` uses) | `41700+i` | 41700 | 41701 | 41702 |
| `tsgw_hostname` | `cpxy-<name>` | cpxy-main | cpxy-guest | cpxy-iot |
| `tsgw_state_dir` | `/etc/cpxy/tsgw/<name>` | | | |

Set one of these in an instance only if it clashes with something else on the router; these are
never taken from `defaults`.

### Options

| Option | Default | Meaning |
|---|---|---|
| `server` | none, **required** | The cpxy server URL with its key. Kept out of the process list. |
| `enabled` (instance) | `1` | `0` stops the whole instance. |
| `network` (instance, list) | none | OpenWrt networks whose devices use the instance. Without one, the instance runs only its proxy and exit node. |
| `client`, `router`, `tsgw` | `1` | Which services the instance runs. |
| `client_socks5` | `0` | `1` adds a SOCKS5 proxy. |
| `ipv6` | `full` | IPv6 from the networks and Tailscale clients. `full` routes it like IPv4, which needs a server that can reach IPv6; `direct` only what goes direct (local-region and private addresses), for a server that cannot; `off` refuses it, so devices use IPv4. The router's WAN needs IPv6, and its networks IPv6 addresses (OpenWrt hands them out by default). |
| `proxy_route` (list) | none | IPv4 or IPv6 CIDRs whose TCP goes through the server even when it would go direct. |
| `dns_upstream` (list) | `223.5.5.5` (AliDNS) | DNS servers whose answer is used, trimmed to its local-region addresses, when it has any. The fastest ones are usually your ISP's. |
| `dns_alternative` (list) | `tcp://1.1.1.1?via=cpxy` | DNS servers whose answer is used otherwise, here asked through the instance's server. |
| `dns_search_domain` (list) | empty | Ordered suffixes for single-label names, shared by router DNS and Tailscale gateway DNS. Tries each suffix before the bare name. |
| `dns_cache` | `1` | dns_split's answer cache, in RAM. |
| `dns_local_domain` (list) | dnsmasq's `domain` and `local` | Domains dns_split leaves to the router (`dns_local_server`), along with single-label names and reverse lookups of private addresses. |
| `dns_local_server` | `127.0.0.1:53` | The router's own resolver, dnsmasq. |
| `router_dns` | `1` | Answer all IPv4 DNS from the instance's networks with its dns_split. |
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

With `router_dns` on, the firewall sends every IPv4 DNS query from the instance's networks (to
the router or to any other server) to the instance's dns_split, and refuses IPv6 DNS so devices
ask over IPv4; dns_split answers AAAA queries there according to `ipv6`. dns_split asks `dns_upstream` and `dns_alternative` at once and uses the upstream
answer only when it has addresses in the local region, keeping just those, so a tampered answer
can never send a blocked site direct.

Each instance's DNS stays inside it: its dns_split asks its overseas servers through its own
server. The router's own lookups never go through cpxy, so nothing here depends on itself to
start.

Because DNS is redirected per firewall zone, a zone holding one instance's networks must not hold
networks of another instance or of none (the service says so). Set `router_dns` to `0` for such an
instance: its devices then use dnsmasq as before.

Names dnsmasq answers specially outside those domains (an `address=/name/` entry, or GL.iNet's
`console.gl-inet.com`) are resolved publicly for redirected networks; add their domains to
`dns_local_domain` to keep dnsmasq's answer.

dns_split runs inside the instance's `cpxy-router`, so while that is down or restarting, its
networks have neither TCP nor DNS until procd starts it again.

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

- **TCP fails closed.** If a packet engine stops, its networks' TCP stops rather than going out
  directly, until procd restarts it.
- **Only TCP goes through the server.** QUIC is refused so browsers fall back to TCP. Other UDP
  (calls, games) goes out directly.
- **IPv6 follows `ipv6`.** With `full`, a name whose IPv4 is in the local region gets no IPv6
  addresses outside it, so it still goes direct. With `direct`, AAAA queries are answered only with
  addresses that go direct, and IPv6 that would go through the server is refused, so devices use
  IPv4 for it.
- **Only the networks' devices are proxied**, not the router itself.
- **Hardware flow offloading** (a GL.iNet option) may bypass the tunnel; cpxy-router warns when
  it is on and an instance routes networks.

## Troubleshooting

| Symptom | Check |
|---|---|
| Nothing starts | `cpxy-show` names what is missing or clashing. |
| A network's devices are not proxied | `logread -e cpxy` for "has no device yet"; check `network` against `uci show network`. |
| A network has no internet | The server URL or key is wrong, or the server is unreachable: `logread -e cpxy-router`. |
| Names do not resolve | `logread -e cpxy-router`: it shows each DNS server it uses, and the errors reaching them or the instance's server. |
| Local names (`printer.lan`) do not resolve | `dns_local_domain`, and that dnsmasq answers on `dns_local_server`. |
| The exit node does not appear | It is not logged in or not approved: `logread -e cpxy-tsgw`. |
