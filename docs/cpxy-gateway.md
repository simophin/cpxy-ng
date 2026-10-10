# cpxy-gateway: user guide

`cpxy-gateway` is a Debian package that sends traffic a Linux box forwards through a cpxy server.
Its main use is a **Tailscale exit node**: phones and laptops pick the box as their exit node in
the Tailscale app and get cpxy routing and split DNS, with no cpxy app on the device.

```text
phone ── Tailscale ──► exit node (tailscale0)
                         ├─ TCP ────────► cpxy0 ► packet engine ► cpxy server
                         │                                (local-region destinations: direct)
                         ├─ UDP 443 (QUIC) ► refused, so apps fall back to TCP
                         ├─ other UDP ───► out directly
                         └─ DNS ─────────► the box's resolver = cpxy-dns-split
```

It uses the same packet engine and `dns_split` resolver as the OpenWrt package
([cpxy-router](cpxy-router.md)), and the same routing script.

Setting up a new box? Follow the [step-by-step setup guide](cpxy-gateway-setup.md); this page is
the reference.

## Before you start

- A Debian 12 or 13 or Ubuntu 24.04 box, `amd64` or `arm64`, with systemd.
- Tailscale installed, logged in and **advertising itself as an exit node**, with the exit node
  approved in the admin console:
  ```sh
  tailscale up --advertise-exit-node
  ```
  See [Tailscale's exit node guide](https://tailscale.com/kb/1103/exit-nodes).
- A running cpxy server, and its URL with the key: `https://:<key>@<host>:<port>` (or `http://`).

## Install

Download `cpxy-gateway_<version>_<arch>.deb` from the
[GitHub releases](https://github.com/simophin/cpxy-ng/releases), then:

```sh
sudo apt install ./cpxy-gateway_*.deb
sudoedit /etc/cpxy/cpxy.conf          # set SERVER; check the DNS servers
sudo systemctl enable --now cpxy-dns-split cpxy-router
```

The package is not signed and there is no apt repository yet: install each new version the same
way. Installing does nothing on its own; the services start once you enable them.

## Settings

`/etc/cpxy/cpxy.conf` holds `KEY=value` lines (it is readable by root only, as it holds the key).
Apply changes with `sudo systemctl restart cpxy-router cpxy-dns-split`. Upgrades keep your edits.

| Setting | Default | Meaning |
|---|---|---|
| `SERVER` | none, **required** | The cpxy server URL, including the key. |
| `INTERFACES` | `tailscale0` | Interfaces whose forwarded traffic is proxied, separated by spaces or commas. A WireGuard interface or LAN bridge works too. |
| `MTU` | `1280` | Largest packet the engine sends back to clients. It must not exceed the smallest MTU on the way back to them; `tailscale0`'s is 1280. Too high and pages stall partway. |
| `DNS_UPSTREAM` | `223.5.5.5,tcp://119.29.29.29` | DNS servers whose answer is used when every address in it is in the local region. |
| `DNS_ALTERNATIVE` | `https://dns.google/dns-query?ip=8.8.8.8` | DNS servers whose answer is used otherwise. A resolver on your tailnet is the reliable choice (see [Choosing the servers](#choosing-the-servers)). |
| `DNS_SYSTEM` | `1` | `1` makes `cpxy-dns-split` the box's resolver (see [DNS](#dns)). `0` leaves `/etc/resolv.conf` alone. |
| `DNS_LISTEN` | `127.0.0.1:53` | Where `cpxy-dns-split` listens. Must be port 53 with `DNS_SYSTEM=1`. |
| `TAILSCALE_ACCEPT_DNS_OFF` | `1` | Turns off Tailscale's "accept DNS" on this box (see [DNS](#dns)). |
| `DNS_CACHE` | `1` | `1` keeps an answer cache in `/var/cache/cpxy`. |
| `TUN`, `ROUTING_TABLE`, `RULE_PRIORITY` | `cpxy0`, `100`, `9100` | Change only if they clash with something else. The priority and the next three are used. |
| `TUN_ADDRESS` | `198.18.0.1/30` | Tailscale masquerades what it forwards, which needs an address on the TUN. |

DNS servers are a plain IP (with an optional `:port`), or a `udp://`, `tcp://`, `tls://` or
`https://` URL; `tls://` and `https://` accept `?ip=<addr>` so no lookup is needed at startup.

## DNS

A Tailscale exit node answers its clients' DNS with **the box's own resolver**. So with
`DNS_SYSTEM=1`, `cpxy-dns-split` takes over `/etc/resolv.conf` (pointing it at `127.0.0.1`) once it
is listening, and puts the original back whenever it stops, crashes included: a broken resolver
costs the split, not DNS. The original file (or systemd-resolved's symlink) is kept in
`/var/lib/cpxy` meanwhile.

Tailscale's "accept DNS" setting (`tailscale set --accept-dns`) would make Tailscale rewrite
`/etc/resolv.conf` itself, so `cpxy-dns-split` turns it off on this box when it starts and logs
that it did. This affects only this machine: MagicDNS and DNS settings for the rest of the tailnet
are unchanged, and `.ts.net` names still work on every other device. Set
`TAILSCALE_ACCEPT_DNS_OFF=0` to manage it yourself.

Keep the admin console's DNS nameservers without **Use with exit node**; otherwise clients send
DNS to those servers instead of the exit node.

### Choosing the servers

The split works by asking both groups at once: an upstream answer is used when every address in it
is in the local region, and the alternative answer otherwise. Where DNS is tampered with, blocked
names get fake addresses outside the local region, so a tampered upstream answer can never send a
blocked site direct. What has to be trustworthy is the **alternative** answer:

- Plain DNS to an overseas server (`DNS_ALTERNATIVE=1.1.1.1`) is poisoned on the way back. Clients
  then connect to fake addresses, and the log shows the server failing to reach them (`Error
  connecting to upstream`).
- DNS over HTTPS or TLS works only while its server is reachable, and some, `dns.google` among
  them, are blocked by name.
- **A resolver on your tailnet** is reached inside Tailscale's encrypted tunnel, so it can be neither
  poisoned nor blocked. Run one on the cpxy server, so CDNs answer for where traffic leaves, and set
  `DNS_ALTERNATIVE=<its Tailscale IP>:<port>`. The [setup guide](cpxy-gateway-setup.md#4b-recommended-an-overseas-resolver-on-your-tailnet)
  shows how.

For `DNS_UPSTREAM`, the resolvers your router hands out over DHCP (`resolvectl status`) are usually
the fastest and give the nearest CDN nodes for your ISP. Check that each server you list answers: a
dead one costs nothing visible but means local-region names wait for the alternative answer.

## Check it

```sh
systemctl status cpxy-router cpxy-dns-split
journalctl -u cpxy-router -u cpxy-dns-split -f     # every proxied connection and DNS answer
ip rule show                                         # rules at priorities 9100-9103
cat /etc/resolv.conf                                  # "Managed by cpxy-dns-split"
```

Then on a phone, choose the box as exit node in the Tailscale app, open a site that should go
through the server, and check the reported public IP is the server's.

## Day-to-day use

**Turn it off temporarily:** `sudo systemctl stop cpxy-router cpxy-dns-split`. Exit-node traffic
goes out directly again and the original `/etc/resolv.conf` is back. `start` brings it back.

**Upgrade:** `sudo apt install ./cpxy-gateway_<newer>.deb`. Running services restart on the new
version; your settings are kept.

**Uninstall:** `sudo apt remove cpxy-gateway` stops everything and restores the original routing
and DNS. `apt purge` also deletes `/etc/cpxy/cpxy.conf` and the cache.

## Things to know

- **It fails closed.** If the packet engine dies, it is restarted within seconds and forwarded
  TCP is refused meanwhile, rather than going out directly. Only stopping the service restores
  direct forwarding.
- **Only TCP goes through the server, over IPv4.** QUIC is refused so apps fall back to TCP. Other
  UDP (video calls, games) goes out directly from the box. IPv6 from clients is refused.
- **Only forwarded traffic is proxied.** The box's own traffic (apt, Tailscale itself) goes direct.
- **"Local region" is decided where the box is.** Local-region destinations, and DNS answers from
  `DNS_UPSTREAM`, go direct from the box. On a box outside the local region, the upstream servers
  return addresses outside it, so the split mostly picks `DNS_ALTERNATIVE` there; it works as
  intended on a box inside the region.
- **A firewall that drops forwarded traffic** (ufw, or an nftables policy of drop) also needs
  forwarding from the interfaces to `cpxy0` allowed, e.g. `ufw route allow in on tailscale0 out on cpxy0`.

## Troubleshooting

| Symptom | Check |
|---|---|
| Services fail to start | `journalctl -u cpxy-router -u cpxy-dns-split`: a missing setting is named there. |
| Clients have no internet | The server URL or key is wrong, or the server is unreachable: see `journalctl -u cpxy-router`. `systemctl stop cpxy-router` restores direct access meanwhile. |
| Pages start loading, then stall | `MTU` is higher than the clients' link; the log warns when an interface's MTU is lower. |
| Blocked sites fail; the log shows `Error connecting to upstream` | `DNS_ALTERNATIVE` is being poisoned: `journalctl -u cpxy-dns-split` shows the name answered from the alternative server with a wrong address. Use a [resolver on your tailnet](#choosing-the-servers). |
| Clients do not get split DNS | `/etc/resolv.conf` should say "Managed by cpxy-dns-split", and `tailscale debug prefs` should show `"CorpDNS": false`. Another resolver on port 53 (dnsmasq, bind) stops `cpxy-dns-split` from starting. |

## Building and testing

`packaging/debian/build-deb.sh <amd64|arm64> <version> <bin dir> <out dir>` assembles the package
from static `cpxy-router` and `cpxy-dns-split` binaries; it needs only `dpkg-deb`. CI builds it for
both architectures alongside the OpenWrt package and attaches it to releases.

- `packaging/debian/test/lab.sh <deb> <dir with cpxy-server>` runs the package's routing with the
  real binaries in network namespaces, with a stand-in `tailscale0` (MTU 1280, masquerading): proxying,
  a large download at that MTU, direct local-region traffic, fail-closed engine restarts and clean
  removal.
- `packaging/debian/test/dpkg-roundtrip.sh <deb>` installs, reinstalls, removes and purges it in
  Debian 12, Debian 13 and Ubuntu 24.04 containers.
