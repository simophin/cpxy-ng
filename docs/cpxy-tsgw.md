# cpxy-tsgw: user guide

`cpxy-tsgw` is a **Tailscale exit node in one program**. Phones and laptops pick it as their exit
node in the Tailscale app and get cpxy routing and split DNS, with no cpxy app on the device.

It runs entirely in user space: it is its own Tailscale node (built on
[tsnet](https://tailscale.com/kb/1244/tsnet)), so it needs no `tailscaled`, no TUN device, no
routing or firewall changes, no IP forwarding and no root. Inside it, the packets clients send to
the internet go to the same packet engine and `dns_split` resolver as the mobile app and
[cpxy-router](cpxy-router.md).

```text
phone ── Tailscale ──► cpxy-tsgw (tsnet: WireGuard, NAT traversal, DERP)
                         ├─ TCP ────────► packet engine ► cpxy server
                         │                         (local-region destinations: direct)
                         ├─ UDP 443 (QUIC) ► refused, so apps fall back to TCP
                         ├─ other UDP ───► out directly
                         ├─ DNS to any server ► split DNS
                         └─ the Tailscale app's DNS (sent to the exit node's peer API) ► split DNS
```

## How it differs from cpxy-gateway

[cpxy-gateway](cpxy-gateway.md) turns a box that already runs `tailscaled` into an exit node: it
adds a TUN device, policy routing and takes over `/etc/resolv.conf`, and needs root and IP
forwarding. `cpxy-tsgw` replaces all of that with one unprivileged service and touches nothing on
the box. It is a separate tailnet node, so it can run on a box that also runs `tailscaled` (or
cpxy-gateway); the two appear as two machines in the admin console.

Choose cpxy-gateway when the box should proxy what other interfaces forward too (a LAN bridge, a
WireGuard interface). For a plain Tailscale exit node, cpxy-tsgw is simpler.

## Before you start

- A Linux box, `amd64` or `arm64`, with systemd for the Debian package. The binary is static and
  runs on any Linux distribution.
- A Tailscale account with admin rights on the tailnet, to approve the exit node.
- A running cpxy server, and its URL with the key: `https://:<key>@<host>:<port>` (or `http://`).

## Install

Download `cpxy-tsgw_<version>_<arch>.deb` from the
[GitHub releases](https://github.com/simophin/cpxy-ng/releases) (or the `tsgw-debian-package-amd64`
/ `-arm64` artifact of a CI run on `main`), then:

```sh
sudo apt install ./cpxy-tsgw_*.deb
sudoedit /etc/cpxy/cpxy-tsgw.conf          # set SERVER; check the DNS servers
sudo systemctl enable --now cpxy-tsgw
journalctl -u cpxy-tsgw -f                  # shows the login link
```

Without the package, run the static binary (`cpxy-tsgw-<arch>-unknown-linux-musl` in the releases)
with the settings below in its environment, or in a file passed with `--config`.

## Log in and approve the exit node

1. **Log in.** On first start the log shows
   `To start this tsnet server, restart with TS_AUTHKEY set, or go to: https://login.tailscale.com/a/...`.
   Open the link and sign in. To log in without a browser, create an
   [auth key](https://login.tailscale.com/admin/settings/keys), set `TS_AUTHKEY=tskey-auth-...` in
   the settings and restart; it is only used once.
2. **Approve the exit node.** In the [admin console](https://login.tailscale.com/admin/machines),
   find the machine (`cpxy-tsgw` unless you set `TS_HOSTNAME`), open its **⋯** menu, choose
   **Edit route settings** and tick **Use as exit node**. Until then the log says
   `the exit node is NOT approved yet`; within half a minute of approving it says
   `the exit node is approved`. (An [auto-approver](https://tailscale.com/kb/1337/acl-syntax#autoapprovers)
   in the tailnet policy does this for you.)
3. **Disable key expiry** in the same menu, so the node does not drop off the tailnet in a few
   months.
4. **DNS:** under **DNS** in the admin console, make sure no global nameserver has **Use with exit
   node** on. Otherwise clients send their DNS there instead of to the exit node, and miss the split
   DNS.

The node's identity is kept in `/var/lib/private/cpxy-tsgw` (the service's state directory), so it
stays logged in across restarts and upgrades.

## Settings

`/etc/cpxy/cpxy-tsgw.conf` holds `KEY=value` lines and is readable by root only, as it holds the
key; systemd reads it before starting the service. Apply changes with
`sudo systemctl restart cpxy-tsgw`. Upgrades keep your edits.

| Setting | Default | Meaning |
|---|---|---|
| `SERVER` | none, **required** | The cpxy server URL, including the key. Never a command-line flag, so it stays out of `ps`. |
| `TS_HOSTNAME` | `cpxy-tsgw` | The node's name on the tailnet. Also `--hostname`. |
| `TS_AUTHKEY` | none | An auth key for the first login instead of the link in the log. |
| `TS_PORT` | random | The UDP port WireGuard listens on. Set it to forward that port on your router, so devices connect directly more often; Tailscale also asks the router itself over UPnP, NAT-PMP or PCP when it can. |
| `CONTROL_URL` | Tailscale's | A different coordination server, e.g. [Headscale](https://headscale.net). Also `--control-url`. |
| `DNS_UPSTREAM` | `223.5.5.5,tcp://119.29.29.29` | DNS servers whose answer is used when every address in it is in the local region. |
| `DNS_ALTERNATIVE` | `https://dns.google/dns-query?ip=8.8.8.8` | DNS servers whose answer is used otherwise. A resolver on your tailnet is the reliable choice (see [Choosing the servers](cpxy-gateway.md#choosing-the-servers)). |
| `BLOCK_ADS` | `0` | `1` answers names on the built-in ad blocklist with an address that goes nowhere. |
| `MTU` | `1280` | The largest packet sent back to clients; Tailscale's MTU is 1280. |
| `STATE_DIR` | the service's state directory | Where the Tailscale state is kept. Also `--state-dir`. |
| `TS_NO_LOGS_NO_SUPPORT` | unset | `true` stops uploading Tailscale's own logs to Tailscale, as for `tailscaled`. |

DNS servers are a plain IP (with an optional `:port`), or a `udp://`, `tcp://`, `tls://` or
`https://` URL; `tls://` and `https://` accept `?ip=<addr>` so no lookup is needed at startup. The
same advice as for cpxy-gateway applies; see [its DNS section](cpxy-gateway.md#choosing-the-servers).

`--verbose` adds Tailscale's own debug output to the log, including each DNS query clients send
over the peer API (`peerapi: exitdns: ...`).

## DNS

Clients using an exit node send their DNS to it in two ways, and both get split DNS:

- The Tailscale app sends it as DNS over HTTPS to the exit node's peer API. `tailscaled` would
  answer it from the box's `/etc/resolv.conf`; cpxy-tsgw answers it with `dns_split`, applying
  Tailscale's own checks on who may ask.
- DNS that apps send to a server of their own (UDP or TCP port 53, to any address) reaches the
  exit node as packets, and the engine answers it the same way.

DNS over TLS (TCP 853) is refused so devices fall back to plain DNS; DNS over HTTPS that apps do
themselves is ordinary HTTPS traffic and goes through the server like any other.

## Check it

```sh
systemctl status cpxy-tsgw
journalctl -u cpxy-tsgw -f       # login link, approval state, every DNS answer and proxied connection
```

Then on a phone, choose the node as exit node in the Tailscale app, open a site that should go
through the server, and check the reported public IP is the server's.

## Things to know

- **It fails closed.** If the packet engine stops, cpxy-tsgw exits and systemd restarts it within
  seconds; clients' traffic is not sent out directly meanwhile.
- **Only TCP goes through the server, over IPv4.** QUIC is refused so apps fall back to TCP. Other
  UDP (video calls, games) goes out directly from the box. IPv6 from clients is refused, and AAAA
  queries get no answers, so apps use IPv4.
- **Local-region destinations go direct** from the box, and so do private addresses. Tailscale
  itself keeps clients off the box's own LAN, as for any exit node.
- **The peer API serves only DNS.** Taildrop to this node and the peer API's debug pages are not
  available; nothing else needs them on an exit node.
- **Only clients' traffic is proxied.** The box's own traffic, cpxy-tsgw's connection to the cpxy
  server and to Tailscale included, goes direct.

## Day-to-day use

**Turn it off temporarily:** `sudo systemctl stop cpxy-tsgw`. Clients using it as exit node lose
internet access until they pick another one or it starts again.

**Upgrade:** `sudo apt install ./cpxy-tsgw_<newer>.deb`. A running service restarts on the new
version; the settings and the login are kept.

**Uninstall:** `sudo apt remove cpxy-tsgw` stops it. `apt purge` also deletes the settings and the
node's Tailscale identity; remove the machine in the admin console too.

## Troubleshooting

| Symptom | Check |
|---|---|
| The service fails to start | `journalctl -u cpxy-tsgw`: a missing or invalid setting is named there. |
| No login link in the log | It is already logged in (state in `/var/lib/private/cpxy-tsgw`), or `TS_AUTHKEY` is set. To log in again, stop it, purge or delete that directory and start it. |
| The exit node does not show in the Tailscale app | The log says whether it is approved; approve it in the admin console. The tailnet policy must also let the client use exit nodes (`autogroup:internet`). |
| Clients have no internet | The server URL or key is wrong, or the server is unreachable: the log shows the failed connections. |
| Clients do not get split DNS | No global nameserver may have **Use with exit node** in the admin console. `--verbose` shows each query arriving over the peer API. |
| Blocked sites fail; the log shows `Error connecting to upstream` | `DNS_ALTERNATIVE` is being poisoned. Use a [resolver on your tailnet](cpxy-gateway.md#choosing-the-servers). |
| Slow connections | Tailscale is relaying through DERP rather than connecting directly. Fewer NAT layers in front of the box help, as for any Tailscale node. |

## Building and testing

`gateway/build.sh` builds the Rust engine as a static library and links the Go program to it:

```sh
gateway/build.sh                                  # for this machine (glibc), into target/release
gateway/build.sh x86_64-unknown-linux-musl        # fully static; needs zig on PATH
gateway/build.sh aarch64-unknown-linux-musl
gateway/build.sh test                             # the Go tests, linked to the engine
```

`packaging/debian-tsgw/build-deb.sh <amd64|arm64> <version> <binary> <out dir>` assembles the
package from a static binary; `packaging/debian-tsgw/test/dpkg-roundtrip.sh <deb>` installs,
reinstalls, removes and purges it in Debian and Ubuntu containers. CI does both.

`gateway/internal/e2e` tests a built binary on a real tailnet: it creates ephemeral auth keys with
the Tailscale API, runs cpxy-tsgw, approves its exit node, joins a client that uses it, checks that
a site abroad sees the cpxy server's address and a local-region one this host's, that DNS is
answered over the peer API and as packets, and deletes the devices and keys afterwards:

```sh
CPXY_TSGW_E2E=1 TS_API_KEY=... CPXY_TSGW_SERVER='https://:<key>@<host>' \
  go -C gateway test -count=1 -v ./internal/e2e
```
