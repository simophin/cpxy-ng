# nft redirect router plan

Status: planned. Branch: `feat/redirect-router`.

Replace the OpenWrt router's TUN path for proxied TCP with an nftables `redirect` into a
transparent listener in `cpxy-client`. The kernel terminates LAN-side TCP; the userspace TCP
stack (ipstack), the TUN device, the GeoIP routing tables and the SOCKS5 hop between
`cpxy-router` and `cpxy-client` leave the router's TCP path.

## Why

- The TUN path runs every proxied LAN TCP connection through a userspace TCP stack. Its
  segmentation, windowing and session bookkeeping are ours to maintain (`vendor/ipstack`
  patches, the stuck-session leaks fixed in #170) and cost CPU on small routers.
- The kernel already implements a transparent TCP proxy entry point. With `redirect`, a
  redirected connection is an ordinary accepted socket whose original destination is read with
  `SO_ORIGINAL_DST`.
- The GeoIP split currently lives in route generations, staging tables and five policy-rule
  priorities per instance. An nft interval set replaced in one `nft -f` transaction holds the
  same policy with less lifecycle machinery.

## Why redirect, not TPROXY

TPROXY delivers without NAT and supports UDP and IPv6 natively, but needs `NF_TPROXY`/`NFT_TPROXY`
in the kernel. The TR3600 firmware lacks them, and enabling them changes vmlinux exports
(`udp4_lib_lookup`), so they cannot be added as modules; a reflash is required. For the current
policy (IPv4 TCP only, UDP direct, no upstream source spoofing) `redirect` is equivalent, needs only
`kmod-nft-nat` (already a firewall4 dependency), and per-packet cost is dominated by the relay
either way. The listener is written so a TPROXY mode is a small later addition: only the way the
original destination is obtained (`getsockname()` instead of `SO_ORIGINAL_DST`) and the socket
option (`IP_TRANSPARENT`) differ.

## Agreed behavior (unchanged for clients)

- CN/private IPv4 TCP forwards directly; other IPv4 TCP is proxied through the instance's cpxy
  server. `proxy_route` still forces CIDRs through the proxy.
- All UDP goes directly except UDP/443, which is rejected. IPv4 ICMP direct.
- Routed network clients are IPv4-only: AAAA answers are empty and IPv6 forwarding is refused.
- DNS on UDP/TCP 53 from the instance's zones is redirected to the instance's `dns_split`.
- dnsmasq and the router's own outbound sockets are left alone; nothing the router originates is
  intercepted.
- Proxied TCP never falls back to WAN: not when cpxy-client is down, not before the rules exist,
  not during a reload.

## Design

### 1. cpxy-client: transparent listener

- New `client/src/transparent_proxy_server.rs` with `TransparentHandshaker`, a third
  `Handshaker` beside the HTTP and SOCKS5 ones, so `serve_listener` and the outbound path are
  reused unchanged.
  - `accept()`: read the original destination with socket2's `original_dst()`
    (`original_dst_ipv6()` for IPv6 sockets) and return `ProxyRequest::WithIP`.
  - Loop guard: no original destination, or one equal to the listener's own address (a client
    connected to the port directly), is refused.
  - `respond_ok()` returns the stream unchanged; `respond_err()` closes it. There is no
    protocol to answer.
- `standalone`: `--transparent-proxy-listen` / `TRANSPARENT_PROXY_LISTEN`, started in the existing
  `try_join!`. Linux only; elsewhere the option is rejected at startup.
- The server connects by IP, as the TUN path does today. Recovering a hostname from TLS SNI is
  out of scope.

### 2. cpxy-router: DNS-only worker

- `--tun` becomes optional. Without it the binary runs only the `dns_split` listener.
  Resolver destinations are still classified in userspace, and proxied resolvers still go
  through the sibling cpxy-client's SOCKS5 listener on loopback, so `--socks5-proxy` stays.
- `--export-direct-routes` is unchanged; its output now feeds an nft set.
- The TUN and ipstack code stays for Android; only the router stops starting it.

### 3. OpenWrt packaging

Replaced, per instance: the TUN device and its `cx1NN` firewall zone, the routing table and its
staging table, the five policy rules from `rule_priority`, route generations (`geo.sh`) and
`net.sh`. In their place one table owned by cpxy, regenerated as a whole in one `nft -f`
transaction on every apply:

```
table ip cpxy {
	set vpn_direct { type ipv4_addr; flags interval; elements = { <export-direct-routes> } }
	chain redirect {
		type nat hook prerouting priority dstnat + 5;
		iifname { "br-vpn" } meta l4proto tcp tcp dport != 53 ip daddr != @vpn_direct redirect to :5100
	}
	chain guard {
		type filter hook forward priority filter - 5;
		iifname { "br-vpn" } meta l4proto tcp ip daddr != @vpn_direct reject with tcp reset
	}
}
```

- Priority `dstnat + 5` runs after fw4's own DNAT chain, so the instance's DNS redirect
  (`cpxy_inst_<n>_dns*`) wins for TCP/53; `tcp dport != 53` makes that explicit.
- `guard` catches anything not redirected that would otherwise be forwarded, so a missing or
  failed redirect rule refuses rather than leaks. Redirected packets are input, not forward, and
  never reach it.
- fw4 accepts redirected connections on input through its generic `ct status dnat accept`;
  validation must confirm this for zones whose input policy is REJECT, otherwise firewall.sh adds
  an explicit input rule for the listener port.
- The listener binds `0.0.0.0:<port>`; it is not reachable from WAN (zone input policy) and the
  loop guard refuses direct LAN connections.
- Boot: `boot-firewall.sh` keeps its forward barrier until the `cpxy` table exists, checking for
  the table instead of the policy rules.
- UDP/443 refusal and IPv6 forwarding refusal move from policy rules into the same table, as
  rules in a forward-hook chain (`udp dport 443 reject`, and an `ip6` table or `inet` chain
  rejecting forwarded IPv6 from the instance's devices).
- `cpxy-client` always runs for a routed instance and gets `TRANSPARENT_PROXY_LISTEN`, plus the
  loopback SOCKS5 listener the DNS worker uses for proxied resolvers.

Configuration changes (docs/openwrt.md):

| Option | Change |
|---|---|
| `client_transparent_listen` | New, default `0.0.0.0:5100+i`. |
| `tun`, `routing_table`, `rule_priority` | Obsolete; ignored with a log line. |
| `router_socks5_proxy`, `client '0'` with a routed network | Rejected: redirect needs the sibling client. External-only SOCKS5 instances are dropped. |
| `router_geoip` | Still required to be `1`. |

The flow-offload checks stay for now. Direct traffic is plain forwarding under this design, so
software flow offload may become compatible; that is a separate, measured change.

Migration: the init script removes an old instance's TUN, tables and rules when it finds TUN-era
state in `/var/run/cpxy` or `/etc/cpxy/router-guards`, installing the new guard first.

## Commit stages

- [ ] Plan (this document).
- [ ] cpxy-client transparent listener and `TRANSPARENT_PROXY_LISTEN`, with unit tests for the
  loop guard and request mapping, and a Linux namespace test of `SO_ORIGINAL_DST` through a real
  `redirect` rule where the environment allows it.
- [ ] cpxy-router DNS-only mode (`--tun` optional).
- [ ] OpenWrt packaging: nft table generation, guard, boot barrier, client wiring, option
  migration; remove `net.sh`/`geo.sh` use from the router.
- [ ] Tests: `init-dryrun.sh`, `lab.sh` and `geo-lab.sh` updated for the nft design; docs.
- [ ] Router validation on TR3600, then PR ready for review.

## Validation

- Rust: client and mobile-engine test suites; clippy.
- Namespace lab (real kernel):
  - CN and private destinations forward directly; overseas destinations reach a fake cpxy
    server with the correct original destination.
  - TCP/53 and UDP/53 reach split DNS; UDP/443 rejected; IPv6 forwarding refused.
  - cpxy-client stopped: overseas TCP is refused, nothing leaves on WAN.
  - nft table deleted by hand: the boot barrier or guard refuses, nothing leaks.
  - Reload with a changed `proxy_route`: the set is replaced atomically; established
    connections survive.
  - Router-originated TCP is not intercepted.
- OpenWrt init/UCI dry-run and package install/remove fixtures.
- TR3600: throughput and CPU against the TUN path (`docs/router-performance.md` method), and
  a day of normal use on FN-V.

## Open questions

- Whether fw4's `ct status dnat accept` covers redirected connections in a REJECT-input zone on
  every supported fw4 version (affects whether firewall.sh needs an explicit input rule).
- Whether to keep `router_dns '0'` instances with a TCP-only redirect, or require router DNS.
