# cpxy-ng

**[Downloads](https://cpxy-download.cloudflare-087.workers.dev/)**: the Android app and the OpenWrt package of every build ([latest](https://cpxy-download.cloudflare-087.workers.dev/latest/), [how publishing works](docs/downloads.md))

- [OpenWrt user guide](docs/openwrt.md): proxy, split DNS and LAN routing per network, in one package
- [OpenWrt packaging](packaging/openwrt/README.md)
- [Mobile VPN app development](mobile/README.md)
- [GeoIP data source](geoip-data/SOURCE.md)
- [OpenWrt router DNS design notes](docs/openwrt-router-dns-handoff.md)
- [Mobile VPN app plan](docs/mobile-vpn-plan.md)

## The `cpxy` binary

One static binary holds the server, the client and the split DNS server:

| Command | Does |
|---|---|
| `cpxy server --key <key> --listen 0.0.0.0:3000` | Accepts tunnels from cpxy clients and connects them to their destinations |
| `cpxy client --server http://:<key>@<host>:<port> --http-proxy-listen 127.0.0.1:8080` | Local HTTP CONNECT, SOCKS5 (`--socks5-proxy-listen`) and transparent (`--transparent-proxy-listen`, Linux) proxies. `--routing global` (the default) sends everything through the server; `--routing cn_split --dns-server <ip>` connects to the local region's, private and tailnet addresses directly |
| `cpxy dns --upstream 223.5.5.5 --alternative tcp://1.1.1.1?via=cpxy --server http://:<key>@<host>:<port>` | Split DNS: local-region answers from the upstreams, the rest from the alternatives. `--resolver-routing cn_split --socks5-proxy <addr>` reaches overseas resolvers through a proxy, as the OpenWrt package does |
| `cpxy routes direct [--local-region overseas] [--family ipv6]` | Prints the prefixes `cn_split` connects to directly, e.g. for nftables sets (`cn-direct` is an alias) |
| `cpxy reverse-proxy --server http://:<key>@<host>:<port> --name <name>` | A reverse exit: lets the server send requests out from this machine, over connections it opens to the server. See [Reverse exits](#reverse-exits) |

### Local region

`cn_split` splits destinations on the CN GeoIP boundary. `--local-region` (`LOCAL_REGION`) on
`cpxy client`, `cpxy dns` and `cpxy routes direct` says which side the device is on:

| `--local-region` | Direct | Through the server | Split DNS answer that wins |
|---|---|---|---|
| `cn` (default) | CN, private, tailnet | everything else | an upstream answer with CN addresses |
| `overseas` ("reverse") | everything but CN | CN | an upstream answer with non-CN addresses |

So with `overseas`, `--upstream` is the resolvers near you (e.g. `1.1.1.1`) and `--alternative`
the CN ones, through the server (e.g. `tcp://223.5.5.5?via=cpxy`). `--proxy-route` still forces
its CIDRs through the server in either region. `cpxy client --routing cn_split` needs
`--dns-server`: there is no default, since which resolver suits depends on the region.

Every option can also be set in the environment variable that `cpxy <command> --help` names, or in
a `.env` file in the working directory.

The Docker image `ghcr.io/simophin/cpxy-ng` is the musl binary on `scratch`. It runs `cpxy server`
on port 3000 by default; give another command to run that instead:

```sh
docker run -e KEY=... -p 3000:3000 ghcr.io/simophin/cpxy-ng:main
docker run -e SERVER=http://:<key>@<host>:<port> -p 8080:8080 ghcr.io/simophin/cpxy-ng:main \
  client --http-proxy-listen 0.0.0.0:8080 --routing cn_split --dns-server 223.5.5.5
```

### Reverse exits

A reverse exit sends traffic out from a machine the server can't connect to, such as one behind
NAT or a firewall that blocks inbound connections, over the cpxy protocol both ways. `cpxy reverse-proxy` there keeps a few connections
(`--links`, default 4) parked at the server; a server route `reverse://<name>` sends requests
matching its `Host` pattern out through them:

```sh
# On the server: requests to exit.proxy.example leave through the exit named home
cpxy server --key <key> --listen 0.0.0.0:3000 --socks5-route 'exit.*=reverse://home'
# On the exit's machine
cpxy reverse-proxy --server http://:<key>@proxy.example:3000 --name home
# On a client: everything through the server, and so out of the exit
cpxy client --server http://:<key>@exit.proxy.example:3000 --socks5-proxy-listen 127.0.0.1:1080
```

`exit.proxy.example` is just another name for the server: the client sends it as `Host`, which picks
the route. The server accepts links only for names a `reverse://` route gives. With no link
parked (the exit is down), requests on that route fail rather than leave from the server.

### Internal targets

Anyone with the key picks the targets, so `cpxy server` (for `direct` routes) and `cpxy
reverse-proxy` refuse targets that resolve only to internal addresses: loopback, private,
link-local, CGNAT (which includes Tailscale) and other special-purpose ranges, also when written
as IPv4-mapped or NAT64 IPv6. Only the addresses that pass are connected to, so DNS can't sneak an
internal one through. To open some anyway, give `--allow-target <CIDR or address>` (repeatable or
comma-separated, `ALLOW_TARGETS`). `socks5://` routes leave the check to the SOCKS5 server.

### Fixed proxy routes

Configure IPv4 CIDRs whose TCP connections must use the main proxy, overriding the direct/CN
routing rules. Rules match the destination IP after DNS resolution;
IP destinations are matched directly. Unmatched destinations keep the existing
routing behavior. IPv6 is not supported by the current routing engine.

- `cpxy client --routing cn_split`, `cpxy routes direct` and
  `cpxy dns --resolver-routing cn_split`: repeat
  `--proxy-route 223.5.5.5/32`, or use `--proxy-route 223.5.5.5/32,10.20.0.0/16`. The equivalent
  environment variable is `PROXY_ROUTES=223.5.5.5/32,10.20.0.0/16`. `--routing global` already
  proxies every destination.
- `mobile-engine-linux` and the mobile engine JSON config: add
  `"proxy_routes": ["223.5.5.5/32", "10.20.0.0/16"]`.
- Android: **Settings → Routing → Proxy routes**, one CIDR per line. Save and
  reconnect to apply. Clearing the editor removes all fixed routes. These
  settings apply to every profile, including shared HTTP/SOCKS proxy connections.
- OpenWrt: add `list proxy_route '223.5.5.5/32'` to an instance or the defaults
  section in `/etc/config/cpxy`, then reload the affected services.

Empty lists preserve existing behavior. Invalid CIDRs are rejected. The DNS-only
`cpxy dns` and `cpxy server` do not make destination routing decisions.

### DNS search domains

`cpxy dns --search-domain home.arpa --search-domain corp.example` tries suffixes in order
for single-label queries before falling back to the bare name. The equivalent environment
setting is `SEARCH_DOMAINS=home.arpa,corp.example`. An empty list preserves existing
behavior. Multi-label queries are unchanged; expanded queries follow the usual split DNS,
local-server routing, caching and blocking policy. Successful expanded replies include a CNAME
from the original short name to the expanded name.

The shared VPN engine accepts the same ordered list as `dns_search_domains` in its
JSON configuration. See [mobile settings](mobile/README.md#dns-search-domains) and
[OpenWrt settings](docs/openwrt.md).
