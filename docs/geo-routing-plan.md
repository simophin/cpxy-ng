# Geo routing for the VPN: OpenWrt and Android

Handoff for when the new VPN (our own protocol, a TUN-based daemon) is ready. It covers how
traffic is split: destinations in the local region go direct, everything else goes through the
VPN. On OpenWrt there is one VPN connection per LAN. It records decisions only; the protocol
itself is out of scope.

Both platforms make the split decision with routing, not in the VPN daemon. The daemon only ever
sees traffic meant for the tunnel and forwards it as IP packets; direct traffic never reaches it.

## Goals

- Each OpenWrt network (LAN) set is one **instance** with its own VPN connection, TUN device and
  routing table.
- Local-region destinations go out the WAN directly; all other destinations go through the
  instance's VPN.
- The decision is made in the kernel (nftables + policy routing), not in the VPN daemon.
- The router's own traffic is routed the same way. Only the VPN daemon's own connection to its
  server (and an explicit bypass list) always goes direct.
- Fail closed: when an instance's VPN is down, its traffic for the tunnel is dropped, never sent
  out the WAN. Local-region traffic keeps working.
- Setup is one UCI config file in the same style as `/etc/config/cpxy`. The init script owns all
  kernel and firewall state; users only `uci set` and `uci commit`.

## Firewall marks

One 32-bit mark, with masked fields so other packages' marks (mwan3, pbr, sqm) are untouched:

| Bits         | Meaning                                                              |
|--------------|----------------------------------------------------------------------|
| `0x00800000` | Local-region destination: direct                                     |
| `0x00400000` | Bypass: always direct. Set by the VPN daemon on its server sockets   |
| `0x000f0000` | Instance: `(index + 1) << 16`, index 0..9                            |

Instances are selected by mark, not by input interface, so LAN traffic and router traffic go
through the same rules.

## nftables

Our own table (not fw4's), so firewall reloads do not touch it. The init script loads it, and
reloads it on a `firewall` reload trigger.

```
table inet vpn {
  set direct4 { type ipv4_addr; flags interval; auto-merge; }
  set direct6 { type ipv6_addr; flags interval; auto-merge; }

  # LAN traffic: the instance comes from the input device
  chain pre {
    type filter hook prerouting priority mangle;
    ct direction reply return
    iifname "br-lan"   meta mark set meta mark & 0xfff0ffff | 0x00010000
    iifname "br-guest" meta mark set meta mark & 0xfff0ffff | 0x00020000
    ip  daddr @direct4 meta mark set meta mark | 0x00800000
    ip6 daddr @direct6 meta mark set meta mark | 0x00800000
  }

  # Router traffic. Must be `type route` so a changed mark re-routes the packet.
  chain out {
    type route hook output priority mangle;
    ct direction reply return
    meta mark & 0x00400000 != 0 return
    meta skuid "ddns" meta mark set meta mark | 0x00400000 return
    meta mark & 0x000f0000 == 0 meta mark set meta mark | 0x00010000
    ip  daddr @direct4 meta mark set meta mark | 0x00800000
    ip6 daddr @direct6 meta mark set meta mark | 0x00800000
  }
}
```

- `ct direction reply return`: replies are never marked. Connections from the WAN side (SSH or
  LuCI from WAN, port forwards to LAN hosts, listeners on the router) must reply out the WAN.
- The `iifname` lines are generated from each instance's networks.
- The `meta skuid` lines are generated from `bypass_uid`.
- The instance line in `out` uses `router_instance`. Without one, the line is left out and the
  router's traffic goes direct.
- Sockets that already carry an instance mark (split DNS, below) keep it.

## Policy routing

Per instance `i`, with priority base `P` and table `T` (both derived from the index):

```
P+0  fwmark <inst>/0x000f0000            lookup main suppress_prefixlength 0   # LAN, private, connected
P+1  fwmark <inst|direct>/0x008f0000     lookup main                           # local region -> WAN
P+2  fwmark <inst>/0x000f0000            lookup T                              # the rest -> VPN
```

Table `T`:

```
default dev vpn<i> metric 10
unreachable default metric 1000     # fail closed when the TUN is gone
```

- IPv4 and IPv6 both get every rule and route (`ip -4` and `ip -6`).
- Unmarked traffic (the VPN daemon's bypass sockets, replies) matches no rule and uses main.
- The `local` table (priority 0) still handles loopback and the router's own addresses first.

## Firewall (UCI, owned by the package)

Per instance, staged and synced the way `cpxy_firewall_sync` does it (package-owned sections
only; a reload that changes nothing makes no edits):

- A zone with `device vpn<i>` and `masq 1`.
- Forwarding from each of the instance's LAN zones to that zone. The existing LAN → WAN forwarding
  stays, for local-region traffic.
- `rp_filter=2` (loose) on `vpn<i>`.

Masquerading is needed on **both** the VPN zones and the WAN zone. The source address is picked
by the first route lookup, before the output chain changes the mark. So router traffic re-routed
into a tunnel leaves with the WAN address, and split DNS's local-region queries re-routed to the
WAN leave with the tunnel address. Masquerading rewrites both. IPv6 needs `masq6` for the same
reason.

Warn when hardware flow offloading is enabled; it can bypass the routing decision.

## Split DNS

The existing split DNS stays as it is: it asks a local-region resolver and an overseas resolver
and picks an answer. It gets one addition:

- `--mark <instance mark>`: every upstream socket sets `SO_MARK` before `connect`/`sendto`. Run
  one listener per instance, each with its own instance's mark.

Routing then dispatches its queries automatically: the local-region resolver goes direct through
the direct set, the overseas resolver goes through that instance's VPN (or fails with
`ENETUNREACH` when it is down). LANs whose VPNs exit in different regions get answers for their
own exit.

- Each LAN zone's port 53 is DNAT'd to its instance's split DNS listener.
- dnsmasq's upstream is set to the router instance's split DNS (`server=127.0.0.1#<port>`,
  `noresolv`), so router processes resolve through it too.

## Local-region address list

- One list for both nftables (`direct4`, `direct6`) and split DNS, so DNS and routing never
  disagree about what is local.
- A baseline ships in the package. A periodic refresh downloads the new list and then reloads
  both from the same file.
- The nft reload is atomic: flush and refill the sets in one `nft -f` transaction.

## VPN daemon requirements

- `SO_MARK 0x00400000` (bypass) on every socket to its server.
- It resolves its server's hostname itself, with bypass-marked sockets, and caches the last good
  address. The system resolver goes through split DNS, which needs the tunnel, which is not up
  yet.
- It manages no routes. The init script creates the TUN device, rules and table; the daemon
  attaches to the TUN by name.

## Router bootstrap

- `sysntpd` uses NTP servers inside the local region, so they go direct through the direct set.
  The clock must be right before the VPN can connect, and the router has no RTC.
- While the router's VPN is down, the router has no connectivity outside the local region.
  Setting `router_instance ''` and reloading switches the router's own traffic back to direct.

## Config

```
config defaults 'defaults'
	option server ''
	option ipv6 'full'
	# The instance the router's own traffic uses; '' leaves the router direct
	option router_instance 'home'
	# Processes that always go direct, besides the VPN daemon
	# list bypass_uid 'ddns'
	list dns_upstream '<local-region resolver>'
	# list dns_alternative 'tls://1.1.1.1'

config instance 'home'
	option enabled '1'
	option index '0'
	list network 'lan'
	# Derived from index (i): tun 'vpn<i>', routing_table 100+i, rule_priority 9100+10i,
	# mark (i+1)<<16, dns_listen per instance

config instance 'guest'
	option enabled '1'
	option index '1'
	list network 'guest'
	option server '...'
```

As in cpxy: every `defaults` option can be overridden per instance. Validate everything before a
reload changes routing, and reload on interface events for the instances' networks.

## Android counterpart

Android does not let the app change the system's routing tables, but `VpnService.Builder` decides
which destinations enter the TUN. The split is made there, so local-region traffic never reaches
the app and the system sends it out the underlying network directly.

- Routes into the TUN:
  - Android 13+ (API 33): `addRoute(0.0.0.0/0)` and `addRoute(::/0)`, then
    `excludeRoute(prefix)` for every local-region prefix.
  - Older versions: `addRoute` with the complement of the local-region list (all address space
    minus the local prefixes, as a minimal set of prefixes), for IPv4 and IPv6.
- The app reads only tunnel-bound packets from the TUN and forwards them as IP packets; it needs
  no userspace TCP/IP stack for the direct path.
- Fail closed: the routes stay in place while the VPN service runs. When the tunnel to the server
  is down, packets read from the TUN are dropped, never sent direct.
- `protect()` on every socket to the VPN server (the counterpart of the bypass mark). The daemon
  resolves its server with protected sockets and caches the last good address, as on OpenWrt.
- Split DNS runs in the app, with the TUN's DNS server address pointing at it. Its upstream
  sockets are **not** protected, so routing dispatches them as on OpenWrt: the local-region
  resolver is outside the VPN routes and goes direct, the overseas resolver goes through the TUN
  and the tunnel. Packets from the app's own sockets that enter the TUN are forwarded like any
  others.
- A route change (a new list) needs the VPN interface rebuilt with a new `Builder` and
  `establish()`. Do it on list refresh only, not per connection.

| OpenWrt                                   | Android                                         |
|-------------------------------------------|-------------------------------------------------|
| Direct set → mark → `lookup main`         | Local prefixes kept out of the VPN routes       |
| Table `T`: `default dev vpn<i>`           | Everything else routed into the TUN             |
| Daemon's bypass `SO_MARK`                 | `protect()` on the daemon's server sockets      |
| Split DNS with the instance mark          | Split DNS with unprotected upstream sockets     |

## Shared across platforms

- The local-region address list and its refresh.
- Split DNS.
- One function that turns the list into each platform's form: nft set elements on OpenWrt,
  `excludeRoute` prefixes or complement routes on Android.

## Still open

- The VPN protocol, and how the TUN gets its address.
- The local-region list source and refresh interval.
