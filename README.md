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
| `cpxy client --server https://:<key>@<host>:<port> --http-proxy-listen 127.0.0.1:8080` | Local HTTP, SOCKS5 (`--socks5-proxy-listen`) and transparent (`--transparent-proxy-listen`, Linux) proxies. `--routing global` (the default) sends everything through the server; `--routing cn_split` connects to Chinese, private and tailnet addresses directly |
| `cpxy dns --upstream 223.5.5.5 --alternative tls://1.1.1.1` | Split DNS: local-region answers from the upstreams, the rest from the alternatives |
| `cpxy routes cn-direct [--ipv6]` | Prints the prefixes `cn_split` connects to directly, e.g. for nftables sets |

Every option can also be set in the environment variable that `cpxy <command> --help` names, or in
a `.env` file in the working directory.

The Docker image `ghcr.io/simophin/cpxy-ng` is the musl binary on `scratch`. It runs `cpxy server`
on port 3000 by default; give another command to run that instead:

```sh
docker run -e KEY=... -p 3000:3000 ghcr.io/simophin/cpxy-ng:main
docker run -e SERVER=https://:<key>@<host>:443 -p 8080:8080 ghcr.io/simophin/cpxy-ng:main \
  client --http-proxy-listen 0.0.0.0:8080 --routing cn_split
```

### Fixed proxy routes

Configure IPv4 CIDRs whose TCP connections must use the main proxy, overriding the direct/CN
routing rules. Rules match the destination IP after DNS resolution;
IP destinations are matched directly. Unmatched destinations keep the existing
routing behavior. IPv6 is not supported by the current routing engine.

- `cpxy client --routing cn_split`, `cpxy routes cn-direct` and `cpxy-router`: repeat
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
