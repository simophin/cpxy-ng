# cpxy-tsgw: user guide

`cpxy-tsgw` is a **Tailscale exit node in one program**. Phones and laptops pick it as their exit
node in the Tailscale app and get cpxy routing and split DNS, with no cpxy app on the device.

It runs entirely in user space: it is its own Tailscale node (built on
[tsnet](https://tailscale.com/kb/1244/tsnet)), so it needs no `tailscaled`, no TUN device, no
routing or firewall changes, no IP forwarding and no root. Inside it, the packets clients send to
the internet go to the same packet engine and `dns_split` resolver as the mobile app and the
[OpenWrt package](openwrt.md), which also includes cpxy-tsgw.

```text
phone ── Tailscale ──► cpxy-tsgw (tsnet: WireGuard, NAT traversal, DERP)
                         ├─ TCP ────────► packet engine ► cpxy server
                         │                         (local-region destinations: direct)
                         ├─ UDP 443 (QUIC) ► refused, so apps fall back to TCP
                         ├─ other UDP ───► out directly
                         ├─ DNS to any server ► split DNS
                         └─ the Tailscale app's DNS (sent to the exit node's peer API) ► split DNS
```

It is a separate tailnet node, so it can run on a box that also runs `tailscaled`; the two appear
as two machines in the admin console. (It replaces the former `cpxy-gateway` Debian package, which
needed `tailscaled`, root, a TUN device and policy routing on the box.)

## Before you start

- A Linux box, `amd64` or `arm64`, with Docker, or anything that can run a static binary: it has
  no dependencies, not even a libc.
- A Tailscale account with admin rights on the tailnet, to approve the exit node.
- A running cpxy server, and its URL with the key: `https://:<key>@<host>:<port>` (or `http://`).

## Install

**Docker.** The image `ghcr.io/simophin/cpxy-ng-tsgw` is built from `main` for `amd64` and
`arm64`. Every setting is an environment variable the image declares with its default (see
[Settings](#settings), or `docker image inspect`), so only `SERVER` is needed. The node's
Tailscale identity lives in the volume `/var/lib/cpxy-tsgw`; keep it, or the machine is new on
every recreate:

```sh
docker run -d --name cpxy-tsgw --restart unless-stopped --network host \
  -v cpxy-tsgw:/var/lib/cpxy-tsgw \
  -e SERVER='https://:<key>@<host>:<port>' \
  ghcr.io/simophin/cpxy-ng-tsgw
docker logs -f cpxy-tsgw                    # shows the login link
```

The image blocks ads by default (`BLOCK_ADS=1`); add `-e BLOCK_ADS=0` to turn that off.

`--network host` lets Tailscale see the box's real addresses and reach the router over UPnP or
NAT-PMP, so devices connect directly more often. Behind Docker's own NAT it still works, relayed
or through the box's NAT like any Tailscale node; then set `TS_PORT` and publish it
(`-p 41641:41641/udp`) for direct connections. Nothing in the container
needs root, so `--user` with a volume it can write works too.

**The binary.** `cpxy-tsgw-<arch>-unknown-linux-musl` in the
[GitHub releases](https://github.com/simophin/cpxy-ng/releases) (or the `cpxy-tsgw-<target>`
artifact of a CI run) runs on any Linux, as any user. Give it the settings in its environment, or in
a `KEY=value` file passed with `--config`, and a `STATE_DIR` it can write:

```sh
STATE_DIR=~/.local/state/cpxy-tsgw ./cpxy-tsgw --config cpxy-tsgw.conf
```

Run it from your init system of choice; it exits with an error if the packet engine stops, so have
it restarted on failure. A systemd unit needs only `ExecStart`, `Restart=on-failure`,
`DynamicUser=yes`, `StateDirectory=cpxy-tsgw` (which sets `STATE_DIRECTORY`, also honoured) and an
`EnvironmentFile` with the settings.

## Log in and approve the exit node

1. **Log in.** On first start the log shows
   `Log in by opening https://login.tailscale.com/a/...` and waits. Open the link and sign in.
   To log in without a browser instead, create an
   [auth key](https://login.tailscale.com/admin/settings/keys) and set `TS_AUTHKEY=tskey-auth-...`
   in the settings; it is only used once. If the node's key ever expires, a new link appears in
   the log the same way, and clients are back once someone opens it.
2. **Approve the exit node.** In the [admin console](https://login.tailscale.com/admin/machines),
   find the machine (`cpxy-tsgw` unless you set `TS_HOSTNAME`), open its **⋯** menu, choose
   **Edit route settings** and tick **Use as exit node**. Until then the log says
   `the exit node is NOT approved yet`; within half a minute of approving it says
   `the exit node is approved`. (An [auto-approver](https://tailscale.com/kb/1337/acl-syntax#autoapprovers)
   in the tailnet policy does this for you.)
3. **Disable key expiry** in the same menu, so the node does not need a new login every few
   months.
4. **DNS:** under **DNS** in the admin console, make sure no global nameserver has **Use with exit
   node** on. Otherwise clients send their DNS there instead of to the exit node, and miss the split
   DNS.

The node's identity is kept in `STATE_DIR` (the volume, with Docker), so it stays logged in across
restarts and upgrades.

## Settings

Settings are environment variables, or `KEY=value` lines in a file passed with `--config` (the
environment wins). Keep the file readable by you only, as it holds the key. Changes apply on
restart.

| Setting | Default | Meaning |
|---|---|---|
| `SERVER` | none, **required** | The cpxy server URL, including the key. Never a command-line flag, so it stays out of `ps`. |
| `TS_HOSTNAME` | `cpxy-tsgw` | The node's name on the tailnet. Also `--hostname`. |
| `TS_AUTHKEY` | none | An auth key for the first login instead of the link in the log. |
| `TS_PORT` | random | The UDP port WireGuard listens on. Set it to forward that port on your router, so devices connect directly more often; Tailscale also asks the router itself over UPnP, NAT-PMP or PCP when it can. |
| `CONTROL_URL` | Tailscale's | A different coordination server, e.g. [Headscale](https://headscale.net). Also `--control-url`. |
| `DNS_UPSTREAM` | `223.5.5.5` (AliDNS) | DNS servers whose answer is used when every address in it is in the local region. |
| `DNS_ALTERNATIVE` | `tcp://1.1.1.1?via=cpxy` | DNS servers whose answer is used otherwise, here asked through the cpxy server (see [Choosing the servers](#choosing-the-servers)). |
| `DNS_SEARCH_DOMAINS` | empty | Ordered suffixes for single-label DNS queries, comma or space separated. Tries each suffix before the bare name. |
| `BLOCK_ADS` | `0`; `1` in the Docker image | `1` answers names on the built-in ad blocklist with an address that goes nowhere, so ads and trackers fail to load on every device using the exit node. |
| `MTU` | `1280` | The largest packet sent back to clients; Tailscale's MTU is 1280. |
| `STATE_DIR` | the service's state directory; `/var/lib/cpxy-tsgw` in the Docker image | Where the Tailscale state is kept. Also `--state-dir`. |
| `TS_NO_LOGS_NO_SUPPORT` | unset | `true` stops uploading Tailscale's own logs to Tailscale, as for `tailscaled`. |

DNS servers are a plain IP (with an optional `:port`), or a `udp://`, `tcp://`, `tls://` or
`https://` URL; `tls://` and `https://` accept `?ip=<addr>` so no lookup is needed at startup.
`tcp://`, `tls://` and `https://` accept `?via=cpxy` to connect through the cpxy server; the cpxy
server then looks up a hostname without `?ip=` itself.

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
- **A server asked through the cpxy server** (`?via=cpxy`, the default) can be neither poisoned nor
  blocked, and CDNs answer for where traffic leaves. The cpxy server carries TCP only, so use
  `tcp://`, `tls://` or `https://`.

Every query goes to every server listed, in both groups, so each extra server is another query
(and, through the cpxy server, another connection) for every name. One server per group, as the
defaults have, is usually best.

For `DNS_UPSTREAM`, the resolver your router hands out over DHCP is usually the fastest and gives
the nearest CDN nodes for your ISP. Check that the server you list answers: a dead one costs
nothing visible but means local-region names wait for the alternative answer.

## Check it

```sh
docker logs -f cpxy-tsgw         # login link, approval state, every DNS answer and proxied connection
```

Then on a phone, choose the node as exit node in the Tailscale app, open a site that should go
through the server, and check the reported public IP is the server's.

## Things to know

- **It fails closed.** If the packet engine stops, cpxy-tsgw exits (and Docker or your init system
  restarts it); clients' traffic is not sent out directly meanwhile.
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

**Turn it off temporarily:** `docker stop cpxy-tsgw`. Clients using it as exit node lose internet
access until they pick another one or it starts again.

**Upgrade:** `docker pull ghcr.io/simophin/cpxy-ng-tsgw`, then recreate the container with the
same volume; the login is kept. With the binary, replace the file and restart.

**Uninstall:** remove the container and the volume (or the binary and its `STATE_DIR`), and remove
the machine in the admin console.

## Troubleshooting

| Symptom | Check |
|---|---|
| It exits at once | The log names a missing or invalid setting. |
| No login link in the log | It is already logged in (state in `STATE_DIR`), or `TS_AUTHKEY` is set. To log in again, stop it, delete that directory (or the volume) and start it. |
| The exit node does not show in the Tailscale app | The log says whether it is approved; approve it in the admin console. The tailnet policy must also let the client use exit nodes (`autogroup:internet`). |
| Clients have no internet | The server URL or key is wrong, or the server is unreachable: the log shows the failed connections. |
| Clients do not get split DNS | No global nameserver may have **Use with exit node** in the admin console. `--verbose` shows each query arriving over the peer API. |
| Blocked sites fail; the log shows `Error connecting to upstream` | `DNS_ALTERNATIVE` is being poisoned. Ask it [through the cpxy server](#choosing-the-servers) with `?via=cpxy`. |
| A site or app is missing content | The ad blocklist may be in the way: set `BLOCK_ADS=0` and compare. |
| Slow connections | Tailscale is relaying through DERP rather than connecting directly. Fewer NAT layers in front of the box help, as for any Tailscale node. |

## Building and testing

`gateway/build.sh` builds the Rust engine as a static library and links the Go program to it:

```sh
gateway/build.sh                                  # for this machine (glibc), into target/release
gateway/build.sh x86_64-unknown-linux-musl        # fully static; needs zig on PATH
gateway/build.sh aarch64-unknown-linux-musl
gateway/build.sh test                             # the Go tests, linked to the engine
```

`Dockerfile.tsgw` builds the same static binary inside the `cargo-zigbuild` image, with Go added,
into an image from `scratch` that declares every setting and its default;
`.github/workflows/docker-publish-tsgw.yml` publishes it from `main`. CI also uploads the bare
binaries and attaches them to releases.

`gateway/internal/e2e` tests a built binary on a real tailnet: it creates ephemeral auth keys with
the Tailscale API, runs cpxy-tsgw, approves its exit node, joins a client that uses it, checks that
a site abroad sees the cpxy server's address and a local-region one this host's, that DNS is
answered over the peer API and as packets, and deletes the devices and keys afterwards:

```sh
CPXY_TSGW_E2E=1 TS_API_KEY=... CPXY_TSGW_SERVER='https://:<key>@<host>' \
  go -C gateway test -count=1 -v ./internal/e2e
```
