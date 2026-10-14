# cpxy-ng

- [OpenWrt user guide](docs/openwrt.md): proxy, split DNS, LAN routing and a Tailscale exit node per network, in one package
- [OpenWrt packaging](packaging/openwrt/README.md)
- [cpxy-tsgw user guide](docs/cpxy-tsgw.md): a Tailscale exit node in one container or static binary, no tailscaled needed
- [Docker bundle user guide](docs/docker-bundle.md): the proxy and a Tailscale exit node for a Linux server, in one container
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
- `cpxy-tsgw`: set `PROXY_ROUTES` in the environment or its KEY=value config file.
- `mobile-engine-linux` and the mobile engine JSON config: add
  `"proxy_routes": ["223.5.5.5/32", "10.20.0.0/16"]`.
- Android: **Settings → Routing → Proxy routes**, one CIDR per line. Save and
  reconnect to apply. Clearing the editor removes all fixed routes. These
  settings apply to every profile, including shared HTTP/SOCKS proxy connections.
- OpenWrt: add `list proxy_route '223.5.5.5/32'` to an instance or the defaults
  section in `/etc/config/cpxy`, then reload the affected services.

Empty lists preserve existing behavior. Invalid CIDRs are rejected. The DNS-only
`dns_split` and the proxy server do not make destination routing decisions.
