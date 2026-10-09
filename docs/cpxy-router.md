# cpxy-router: user guide

`cpxy-router` is an OpenWrt package that puts every device on your LAN behind a cpxy server, with no
settings needed on the devices themselves. This guide covers installing, configuring, operating and
removing it. For what the package changes on the router, and how to build and test it, see
[packaging/openwrt/README.md](../packaging/openwrt/README.md).

## Before you start

You need:

- A router on **OpenWrt 24.10** (opkg, fw4, procd), or GL.iNet's stock 4.x firmware (OpenWrt
  21.02-based, fw3). Tested on a GL-MT3000 on both the `-op24` and the stock firmware.
- SSH access as `root`.
- A running cpxy server, and its URL with the key: `https://:<key>@<host>:<port>` (or `http://`).
- Two sets of DNS servers if you keep the DNS feature on (the default): an **upstream** set and an
  **alternative** set (see [DNS](#dns)).

Check the router's architecture:

```sh
opkg print-architecture
```

Packages are built for `aarch64_cortex-a53` and `x86_64`. Other architectures are not built yet.

## 1. Get the package

Download `cpxy-router_<version>_<arch>.ipk` from the
[GitHub releases](https://github.com/simophin/cpxy-ng/releases) page. Builds from `main` are also
available as the `openwrt-package-<arch>` artifact of a CI run.

## 2. Install

Copy the package to the router and install it (GL.iNet's default address is `192.168.8.1`; OpenWrt's
is `192.168.1.1`):

```sh
scp cpxy-router_*.ipk root@192.168.8.1:/tmp/
ssh root@192.168.8.1
opkg update
opkg install /tmp/cpxy-router_*.ipk
```

`opkg update` is needed so opkg can fetch the two dependencies, `ip-full` and `kmod-tun`.

Installing does not change how the LAN reaches the internet yet: the service stays off until you
configure and enable it.

## 3. Configure

Settings live in `/etc/config/cpxy` and are changed with `uci`. The minimum:

```sh
uci set cpxy.main.server='https://:<key>@<host>:<port>'
uci add_list cpxy.main.dns_upstream='<server>'
uci add_list cpxy.main.dns_alternative='<server>'
uci set cpxy.main.enabled='1'
uci commit cpxy
```

### Options

| Option | Default | Meaning |
|---|---|---|
| `enabled` | `0` | `1` to run the service. |
| `server` | none, **required** | The cpxy server URL, including the key. It is kept out of the process list. |
| `lan_interface` (list) | `lan` in the default section | UCI networks whose clients use this section's upstream; firewall source zones are discovered automatically. |
| `tun` | `cpxy0` for `main` | Persistent TUN; required and distinct for additional instances. |
| `routing_table` | `100` for `main` | Routing table; required and distinct for additional instances. |
| `rule_priority` | `9100` for `main` | First of four rule priorities; required and nonoverlapping for additional instances. |
| `dns_split` | `1` for `main`, `0` for others | `1` owns the shared resolver (see [DNS](#dns)); `0` starts no DNS worker for this section. Only one enabled section may own it. |
| `dns_upstream` (list) | none, **required** with `dns_split` | DNS servers whose answer is used when every address in it is in the local region. |
| `dns_alternative` (list) | none, **required** with `dns_split` | DNS servers whose answer is used otherwise. |
| `dns_cache` | `1` | `1` keeps a DNS answer cache in RAM (`/tmp`); `0` disables it. |
| `dns_listen` | `127.0.0.1:5335` | Where the DNS resolver listens. dnsmasq forwards to it. Avoid 5353, the mDNS port. |

A DNS server entry is either a plain IP or a URL:

| Form | Example |
|---|---|
| Plain IP (UDP, port 53) | `223.5.5.5` |
| UDP / TCP (IP only, port defaults to 53) | `udp://8.8.8.8:53`, `tcp://8.8.8.8` |
| DNS over TLS | `tls://dns.google`, `tls://dns.google?ip=8.8.8.8` |
| DNS over HTTPS | `https://dns.google/dns-query`, `https://dns.google/dns-query?ip=8.8.8.8` |

For `tls://` and `https://`, `?ip=<addr>` gives the server's address directly so the resolver does
not have to look the hostname up at startup.

To replace a list rather than add to it, delete it first:

```sh
uci delete cpxy.main.dns_alternative
uci add_list cpxy.main.dns_alternative='tls://dns.google?ip=8.8.8.8'
```

### A second upstream for another SSID

Create the `guest` network/SSID using OpenWrt first. Then add to `/etc/config/cpxy`:

```uci
config cpxy 'guest'
    option enabled '1'
    option server 'http://:<second-key>@<second-host>:80'
    list lan_interface 'guest'
    option tun 'cpxy1'
    option routing_table '101'
    option rule_priority '9110'
    option dns_split '0'
```

Run `/etc/init.d/cpxy reload`. Devices on `guest` use the second server for proxied TCP;
CN destinations still go direct. The service automatically discovers the source firewall
zone, adds forwarding to the second tunnel, and leaves existing network isolation as configured.
Both networks share main's DNS resolver. To disable only the second connection:

```sh
uci set cpxy.guest.enabled='0'
uci commit cpxy
/etc/init.d/cpxy reload
```

Main stays running; guest returns to its original routing. Use `reload` for instance edits:
validation errors preserve running state, and unchanged workers are not restarted.
Legacy `socks5_listen`, `api_listen` and `dns_server` options are ignored by the native engine.

### DNS

With `dns_split` on, dnsmasq keeps serving DHCP and local hostnames, and forwards everything else to
the package's resolver. For each name it asks both server sets: if the `dns_upstream` answer contains
only local-region addresses, that answer is used; otherwise the `dns_alternative` answer is used.

dnsmasq is pointed at the resolver only once it is listening, and back at its usual servers
whenever the resolver is not running, so a resolver that fails to start costs the split, not the
LAN's DNS. `logread -e cpxy-dns` shows why it stopped.

If `dhcp.@dnsmasq[0].server` is set (custom DNS forwarders in LuCI), dnsmasq may still send some
queries there; clear it for all lookups to go through the resolver.

## 4. Start and check

```sh
/etc/init.d/cpxy enable     # start at boot
/etc/init.d/cpxy restart
```

Then check:

```sh
logread -e cpxy             # service messages; a missing required option is reported here
ip link show cpxy0          # the tunnel device exists
ip rule show                # rules at priorities 9100-9103 for each LAN device
```

From a LAN device, browse to a site that should go through the server and one that should not, and
check the public IP each one reports.

## Day-to-day use

**Change a setting:** edit with `uci`, `uci commit cpxy`, then `/etc/init.d/cpxy reload`.

**Turn it off temporarily:** `/etc/init.d/cpxy stop`. The LAN goes direct again, exactly as before
the install. `start` (or a reboot, if enabled) brings it back.

**Turn it off for good, but keep it installed:**

```sh
uci set cpxy.main.enabled='0'
uci commit cpxy
/etc/init.d/cpxy stop
```

**Upgrade:** install the newer `.ipk` the same way as in step 2. `/etc/config/cpxy` is kept.

**Uninstall:** `opkg remove cpxy-router`. This stops the service, removes the firewall zone it added
and restores the original routing and DNS. If you changed `/etc/config/cpxy`, opkg may leave it
behind; delete it if you do not need it.

## Things to know

- **When enabled, it fails closed.** If a component crashes, LAN traffic stops rather than going
  out directly; procd restarts the component within seconds. Use `stop` when you want direct access.
- **Only TCP goes through the server, over IPv4.** QUIC (UDP 443) is refused at once, so browsers
  fall back to TCP through the server. Other UDP (video calls, games, VoIP) goes out directly, so
  those services see your real address. IPv6 from the LAN is refused, so devices use IPv4. DNS to
  the router is unaffected.
- **Only LAN clients are proxied.** Traffic from the router itself (opkg, NTP, …) goes direct.
- **Hardware flow offloading** (a GL.iNet firewall option) may bypass the tunnel. The service logs
  a warning when it is on; turn it off. Software offloading is fine.

## Troubleshooting

| Symptom | Check |
|---|---|
| Nothing changes after `restart` | `uci get cpxy.main.enabled` is `1`, and `logread -e cpxy` has no "is required" or "is not set" message. |
| No LAN device is proxied | `logread -e cpxy` for "has no device yet"; check `lan_interface` names match `uci show network`. |
| LAN has no internet at all | The server URL or key is wrong, or the server is unreachable. Look at `logread -e cpxy`; `stop` restores direct access meanwhile. |
| Names resolve oddly | Check the `dns_upstream` / `dns_alternative` lists and `dhcp.@dnsmasq[0].server`. Set `dns_split` to `0` to rule the resolver out. |
