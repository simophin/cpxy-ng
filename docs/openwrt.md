# cpxy on OpenWrt: user guide

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

- A router on **OpenWrt 24.10** (opkg, fw4, procd), or GL.iNet's stock 4.x firmware (OpenWrt
  21.02-based, fw3). Tested on a GL-MT3000 with the stock firmware. `opkg print-architecture`
  must list `aarch64_cortex-a53` or `x86_64`.
- SSH access as `root`.
- A running cpxy server per instance (one can serve several), and its URL with the key:
  `https://:<key>@<host>:<port>` (or `http://`).
- For more than one instance, the networks (and their SSIDs and firewall zones) already set up in
  OpenWrt, each network in a firewall zone of its own.
- About 60 MB of free flash, and RAM for each instance: about 30 MB, plus about 35 MB for its
  Tailscale exit node. A GL-MT3000 (512 MB) runs three instances with exit nodes.

## Install

Download `cpxy_<version>_<arch>.ipk` from the [releases](https://github.com/simophin/cpxy-ng/releases)
(or the `openwrt-package-<arch>` artifact of a CI run), copy it to the router and install it:

```sh
scp -O cpxy_*.ipk root@192.168.8.1:/tmp/
ssh root@192.168.8.1
opkg update && opkg install /tmp/cpxy_*.ipk      # also pulls in ip-full and kmod-tun
```

If the older `cpxy-router` package is installed, remove it first (`opkg remove cpxy-router`); its
settings do not carry over. A `smartdns` package, if installed, is neither used nor changed.

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
	option block_ads '0'
```

`cpxy-show` prints what each instance runs with.

The `index` gives each instance its own ports, devices and routing numbers:

| Option | Default for index `i` | main (0) | guest (1) | iot (2) |
|---|---|---|---|---|
| `client_http_listen` | `[::]:5000+i` | 5000 | 5001 | 5002 |
| `client_socks5_listen` (with `client_socks5 1`) | `[::]:1080+i` | 1080 | 1081 | 1082 |
| `dns_listen` (dns_split) | `0.0.0.0:5335+10i` | 5335 | 5345 | 5355 |
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
| `dns_upstream` (list) | `223.5.5.5` (AliDNS) | DNS servers whose answer is used when all its addresses are in the local region. The fastest ones are usually your ISP's. |
| `dns_alternative` (list) | `tcp://1.1.1.1?via=cpxy` | DNS servers whose answer is used otherwise, here asked through the instance's server. |
| `dns_search_domain` (list) | empty | Ordered suffixes for single-label names, shared by router DNS and Tailscale gateway DNS. Tries each suffix before the bare name. |
| `dns_cache` | `1` | dns_split's answer cache, in RAM. |
| `block_ads` | `1` | Answer names on the built-in ad blocklist with an address that goes nowhere (dns_split and tsgw). |
| `dns_local_domain` (list) | dnsmasq's `domain` and `local` | Domains dns_split leaves to the router (`dns_local_server`), along with single-label names and reverse lookups of private addresses. |
| `dns_local_server` | `127.0.0.1:53` | The router's own resolver, dnsmasq. |
| `router_dns` | `1` | Answer all IPv4 DNS from the instance's networks with its dns_split. |
| `tsgw_authkey` | none | A Tailscale auth key, to log in without the link. |
| `tsgw_control_url` | Tailscale's | A different coordination server, e.g. Headscale. One whose certificate is not from Let's Encrypt also needs `opkg install ca-bundle`. |
| `tsgw_mtu` | `1280` | The largest packet sent back to Tailscale clients. |
| `tsgw_proxy` | `1` | Reach Tailscale's coordination and relay (DERP) servers through the instance's proxy. |
| `tsgw_no_logs_no_support`, `tsgw_verbose` | `0` | Stop uploading Tailscale's logs; more logging. |

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
use IPv4. dns_split asks `dns_upstream` and `dns_alternative` at once and uses the upstream
answer only when all its addresses are in the local region, so a tampered answer can never send
a blocked site direct.

Each instance's DNS stays inside it: its dns_split asks its overseas servers through its own
server. The router's own lookups never go through cpxy, so nothing here depends on itself to
start.

Because DNS is redirected per firewall zone, a zone holding one instance's networks must not hold
networks of another instance or of none (the service says so). Set `router_dns` to `0` for such an
instance: its devices then use dnsmasq as before.

Names dnsmasq answers specially outside those domains (an `address=/name/` entry, or GL.iNet's
`console.gl-inet.com`) are resolved publicly for redirected networks; add their domains to
`dns_local_domain` to keep dnsmasq's answer.

While an instance's dns_split is down, its networks have no DNS until procd restarts it, just as
their TCP stops while the packet engine is down.

### Tailscale

Each instance with `tsgw` on is its own machine on your tailnet, `cpxy-<name>`. To set one up:

1. `logread -e cpxy-tsgw` shows `Log in by opening https://login.tailscale.com/a/...`; open it.
2. In the [admin console](https://login.tailscale.com/admin/machines), approve it as an exit node
   (**Edit route settings**, **Use as exit node**) and disable its key expiry.

The login is kept in `/etc/cpxy/tsgw/<name>`, across reboots and firmware upgrades. Phones then
pick `cpxy-<name>` as exit node in the Tailscale app. [The cpxy-tsgw guide](cpxy-tsgw.md) has
the details.

## Day-to-day use

- **What runs:** `cpxy-show`, and `logread -e cpxy` for every service's messages.
- **Turn one instance off:** `uci set cpxy.guest.enabled=0; uci commit cpxy`. Its networks go
  direct again; the others keep running.
- **Turn everything off for now:** `for s in cpxy-tsgw cpxy-router cpxy-client; do /etc/init.d/$s stop; done`.
  The networks go direct and use dnsmasq again. `start` (or a reboot) brings them back.
- **Upgrade:** install the newer `.ipk` the same way. `/etc/config/cpxy` and the Tailscale logins
  are kept.
- **Uninstall:** `opkg remove cpxy` stops everything and removes the firewall sections and routing
  it added. A changed `/etc/config/cpxy` and `/etc/cpxy` stay behind; delete them if you do not
  need them, and remove the machines from the Tailscale admin console.

## Things to know

- **TCP fails closed.** If a packet engine stops, its networks' TCP stops rather than going out
  directly, until procd restarts it.
- **Only TCP goes through the server, over IPv4.** QUIC is refused so browsers fall back to TCP.
  Other UDP (calls, games) goes out directly. IPv6 from the networks is refused.
- **Only the networks' devices are proxied**, not the router itself.
- **Hardware flow offloading** (a GL.iNet option) may bypass the tunnel; cpxy-router warns when
  it is on and an instance routes networks.

## Troubleshooting

| Symptom | Check |
|---|---|
| Nothing starts | `cpxy-show` names what is missing or clashing. |
| A network's devices are not proxied | `logread -e cpxy` for "has no device yet"; check `network` against `uci show network`. |
| A network has no internet | The server URL or key is wrong, or the server is unreachable: `logread -e cpxy-router`. |
| Names do not resolve | `logread -e cpxy-dns-split`: it shows each server it uses, and the errors reaching them or the instance's server. |
| Local names (`printer.lan`) do not resolve | `dns_local_domain`, and that dnsmasq answers on `dns_local_server`. |
| A site or app is missing content | The ad blocklist may be in the way: `uci set cpxy.<instance>.block_ads=0`. |
| The exit node does not appear | It is not logged in or not approved: `logread -e cpxy-tsgw`. |
