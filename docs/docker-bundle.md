# cpxy bundle for Docker: user guide

One container runs a whole cpxy bundle for a Linux server, the same set the
[OpenWrt package](openwrt.md) runs per network, minus the LAN routing:

| Service | What it does |
|---|---|
| client | The cpxy HTTP proxy (and optionally SOCKS5) for anything on your network to use |
| tsgw | A [Tailscale exit node](cpxy-tsgw.md) with cpxy routing and split DNS. Its alternative DNS server, 1.1.1.1 over TCP, is asked **through the cpxy server**, so its answers are neither poisoned nor blocked. Tailscale's own traffic goes through the proxy. |

```text
apps ──► client :5000 ─────────────────────────────► cpxy server
          ▲
          └── tsgw (control, DERP)
phone ── Tailscale ──► tsgw ── packet engine ──────► cpxy server
                         └── split DNS alternative ► cpxy server ► 1.1.1.1
```

Both share one server URL. [s6-overlay](https://github.com/just-containers/s6-overlay)
supervises them: a service that dies is restarted on its own, without the other.

## Run it

The image `ghcr.io/simophin/cpxy-ng-bundle` is built from `main` for `amd64` and `arm64`. Only
`SERVER` is needed; every other setting has a default (see [Settings](#settings)).

```sh
docker run -d --name cpxy --restart unless-stopped \
  -p 5000:5000 -v cpxy:/var/lib/cpxy-tsgw \
  -e SERVER='https://:<key>@<host>:<port>' \
  ghcr.io/simophin/cpxy-ng-bundle
docker logs -f cpxy                         # shows tsgw's login link
```

Then log in and approve the exit node as in
[cpxy-tsgw's guide](cpxy-tsgw.md#log-in-and-approve-the-exit-node). The node's Tailscale identity is
kept in the volume `/var/lib/cpxy-tsgw`; keep it, or the machine is new on every recreate.

Tailscale needs no port opened: it connects out and gets through NAT by itself, relaying when it
cannot. Only behind a NAT that defeats that is it worth setting `TS_PORT` and forwarding it
(`-p <port>:<port>/udp`) for direct connections.

The proxy listens on every address inside the container. Publish it only where it should be
reachable, e.g. `-p 192.168.1.10:5000:5000` for one LAN address: anyone who reaches it uses your
cpxy server.

## Several bundles

Each bundle is a container: one per cpxy server, say. Give each its own name, volume, `SERVER` and
`TS_HOSTNAME`.

**On Docker's bridge network** (as above) every container has its own loopback, so only the
published ports differ:

```sh
docker run -d --name cpxy-hk --restart unless-stopped \
  -p 5001:5000 -v cpxy-hk:/var/lib/cpxy-tsgw \
  -e SERVER='https://:<key>@hk.example.com' -e TS_HOSTNAME=server-hk \
  ghcr.io/simophin/cpxy-ng-bundle
```

**On host network** (`--network host`) the containers share the host's ports, so each needs its own
`HTTP_PROXY_LISTEN` (and `SOCKS5_PROXY_LISTEN`, if on). Tailscale's port is random unless set, so it
never clashes:

```sh
docker run -d --name cpxy-hk --restart unless-stopped --network host \
  -v cpxy-hk:/var/lib/cpxy-tsgw \
  -e SERVER='https://:<key>@hk.example.com' -e TS_HOSTNAME=server-hk \
  -e HTTP_PROXY_LISTEN='[::]:5001' \
  ghcr.io/simophin/cpxy-ng-bundle
```

Host network lets Tailscale see the box's real addresses and ask the router for a port (UPnP,
NAT-PMP), so devices connect directly more often.

## Settings

Environment variables, each declared in the image with its default (`docker image inspect` lists
them). A change applies when the container is recreated, which restarts both services.

| Setting | Default | Meaning |
|---|---|---|
| `SERVER` | none, **required** | The cpxy server URL with its key, `https://:<key>@<host>:<port>` (or `http://`). The client and tsgw both use it. |
| `HTTP_PROXY_LISTEN` | `[::]:5000` | The HTTP proxy. tsgw reaches it on loopback at this port. |
| `SOCKS5_PROXY_LISTEN` | empty, off | A SOCKS5 proxy as well, e.g. `[::]:1080`. |
| `DNS_UPSTREAM` | `223.5.5.5` (AliDNS) | tsgw's split DNS: servers whose answer is used, trimmed to its local-region addresses, when it has any. See [choosing the servers](cpxy-tsgw.md#choosing-the-servers). |
| `DNS_ALTERNATIVE` | `tcp://1.1.1.1?via=cpxy` | Servers whose answer is used otherwise. `?via=cpxy` asks a `tcp://`, `tls://` or `https://` server through the cpxy server. |
| `DNS_SEARCH_DOMAINS` | empty | Ordered suffixes for single-label DNS queries from Tailscale clients, comma or space separated. Tries each suffix before the bare name. |
| `BLOCK_ADS` | `1` | tsgw answers names on the built-in ad blocklist with an address that goes nowhere; `0` turns it off. |
| `TS_HOSTNAME` | `cpxy-tsgw` | The exit node's name on the tailnet. |
| `TS_AUTHKEY` | empty | An auth key for the first login instead of the link in the log. |
| `TS_PORT` | empty, random | The UDP port WireGuard listens on, only worth setting to forward it. |
| `CONTROL_URL` | empty, Tailscale's | A different coordination server, e.g. [Headscale](https://headscale.net). |
| `TS_NO_LOGS_NO_SUPPORT` | empty | `true` stops uploading Tailscale's own logs to Tailscale. |
| `MTU` | `1280` | The largest packet tsgw sends back to clients; Tailscale's MTU is 1280. |

The wiring between the services is not a setting: tsgw always uses this container's proxy. Bad
settings stop the container at once, with a line saying which (`docker logs`).

**From the cpxy-tsgw image.** Every setting above that the `cpxy-ng-tsgw` image also has means the
same here, with the same defaults, and the volume is the same, so switching is a change of image
name.

**From an image with smartdns.** Earlier images ran a smartdns for tsgw's alternative DNS. Its
`SMARTDNS_*` settings are now ignored; the default `DNS_ALTERNATIVE` asks the same servers through
the cpxy server instead.

## Check it

```sh
docker ps                                   # STATUS shows (healthy) once both run steadily
curl -x http://<host>:5000 https://ifconfig.co   # the cpxy server's address
```

Each log line is tagged `[client]` or `[tsgw]`; `docker logs cpxy | grep '^\[tsgw\]'` shows one
service.

The container is **unhealthy** when a service is down or keeps restarting: its log lines show why.
The usual causes are a wrong `SERVER` (the client and tsgw log connection errors) or, on host
network, a port another container or program already uses.
