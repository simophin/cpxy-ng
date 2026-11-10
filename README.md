# cpxy-ng

**[Downloads](https://cpxy-download.cloudflare-087.workers.dev/)**: the Android app and the OpenWrt package of every build ([latest](https://cpxy-download.cloudflare-087.workers.dev/latest/), [how publishing works](docs/downloads.md))

- [OpenWrt user guide](docs/openwrt.md): proxy, split DNS and LAN routing per network, in one package
- [OpenWrt packaging](packaging/openwrt/README.md)
- [Mobile VPN app development](mobile/README.md)
- [GeoIP data source](geoip-data/SOURCE.md)
- [OpenWrt router DNS design notes](docs/openwrt-router-dns-handoff.md)
- [Mobile VPN app plan](docs/mobile-vpn-plan.md)

### Fixed proxy routes

Configure IPv4 CIDRs whose TCP connections must use the main proxy, overriding direct/CN, AI and
Tailscale routing rules. Rules match the destination IP after DNS resolution;
IP destinations are matched directly. Unmatched destinations keep the existing
routing behavior. IPv6 is not supported by the current routing engine.

- `client_cn` and `cpxy-router`: repeat `--proxy-route 223.5.5.5/32`, or use
  `--proxy-route 223.5.5.5/32,10.20.0.0/16`. The equivalent environment variable is
  `PROXY_ROUTES=223.5.5.5/32,10.20.0.0/16`.
- `standalone` accepts the same option; it already proxies every destination.
- `mobile-engine-linux` and the mobile engine JSON config: add
  `"proxy_routes": ["223.5.5.5/32", "10.20.0.0/16"]`.
- Android: **Settings → Routing → Proxy routes**, one CIDR per line. Save and
  reconnect to apply. Clearing the editor removes all fixed routes. These
  settings apply to every profile, including shared HTTP/SOCKS proxy connections.
- OpenWrt: add `list proxy_route '223.5.5.5/32'` to an instance or the defaults
  section in `/etc/config/cpxy`, then reload the affected services.

Empty lists preserve existing behavior. Invalid CIDRs are rejected. The DNS-only
`dns_split` and the proxy server do not make destination routing decisions.

### DNS search domains

`dns_split --search-domain home.arpa --search-domain corp.example` tries suffixes in order
for single-label queries before falling back to the bare name. The equivalent environment
setting is `DNS_SEARCH_DOMAINS=home.arpa,corp.example`. An empty list preserves existing
behavior. Multi-label queries are unchanged; expanded queries follow the usual split DNS,
local-server routing, caching and blocking policy. Successful expanded replies include a CNAME
from the original short name to the expanded name.

The shared VPN engine accepts the same ordered list as `dns_search_domains` in its
JSON configuration. See [mobile settings](mobile/README.md#dns-search-domains) and
[OpenWrt settings](docs/openwrt.md).
