#!/bin/sh
# Exercise both init scripts with three instances, and real UCI firewall generation, in an
# OpenWrt rootfs. procd and loading the nftables ruleset are stubbed: commands are logged, not run;
# the ruleset itself is rendered for real.
#
# usage: init-dryrun.sh <ipk> [image]    (needs podman or docker)
set -eu
ipk="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
image="${2:-docker.io/openwrt/rootfs:x86-64-openwrt-24.10}"
engine="$(command -v podman || command -v docker)"
script="$(cat <<'SCRIPT'
set -e
mkdir -p /var/lock /tmp
opkg install /pkg.ipk >/dev/null 2>&1
uci -q delete firewall.@zone[0].network
# Zone names intentionally differ from network names.
uci set firewall.home=zone
uci set firewall.home.name=trusted
uci add_list firewall.home.network=lan
uci add_list firewall.home.network=extra
uci set firewall.visitors=zone
uci set firewall.visitors.name=visitors
uci set firewall.visitors.network=guest
uci set firewall.things=zone
uci set firewall.things.name=things
uci set firewall.things.network=iot
# Zones commonly accept DNS explicitly; the IPv6 DNS refusal must come first
uci set firewall.visitors_dns=rule
uci set firewall.visitors_dns.name=Allow-DNS-visitors
uci set firewall.visitors_dns.src=visitors
uci set firewall.visitors_dns.dest_port=53
uci set firewall.visitors_dns.target=ACCEPT
uci reorder firewall.visitors_dns=0 || { echo "FAIL: reorder"; exit 1; }
uci commit firewall
# A masquerading zone's devices are uplinks even while down
uci set firewall.uplink=zone
uci set firewall.uplink.name=uplink
uci set firewall.uplink.masq=1
uci add_list firewall.uplink.device=wwan9
uci commit firewall
uci set dhcp.@dnsmasq[0].domain=lan
uci set dhcp.@dnsmasq[0].local=/lan/
uci commit dhcp

LOG=/tmp/calls.txt
: >"$LOG"
cat >/etc/init.d/firewall <<'FW'
#!/bin/sh
echo "firewall $*" >>/tmp/calls.txt
FW
chmod +x /etc/init.d/firewall
cat >/usr/libexec/cpxy/boot-firewall.sh <<'FW'
#!/bin/sh
echo "boot_fw $*" >>/tmp/calls.txt
FW
chmod +x /usr/libexec/cpxy/boot-firewall.sh
. /lib/functions.sh
procd_open_instance() { echo "instance $1" >>"$LOG"; }
procd_close_instance() { :; }
procd_set_param() { echo "set $*" >>"$LOG"; }
procd_append_param() { echo "append $*" >>"$LOG"; }
procd_kill() { echo "FAIL: procd_kill would destroy the current service update"; exit 1; }
rc_procd() { "$@"; }
logger() { :; }
fail() { echo "FAIL: $*"; cat "$LOG"; cat /tmp/err 2>/dev/null; exit 1; }
want() { grep -qF -- "$1" "$LOG" || fail "expected: $1"; }
absent() { if grep -qF -- "$1" "$LOG"; then fail "unexpected: $1"; fi; }
# The ruleset the last apply installed
nft_has() { grep -qF -- "$1" /tmp/applied.nft || { cat /tmp/applied.nft | grep -v '^\t\t\t'; fail "ruleset lacks: $1"; }; }
nft_lacks() { if grep -qF -- "$1" /tmp/applied.nft; then fail "ruleset has: $1"; fi; }
# service <name> <function>: runs one init script's function in a shell of its own
service() (
	. "/etc/init.d/$1"
	if [ "$1" = cpxy-router ]; then
		# 'alias' is another network on the guest bridge
		network_get_device() {
			[ "$2" != lan ] || [ ! -f /tmp/no-lan ] || return 1
			case "$2" in lan) eval "$1=br-lan" ;; extra) eval "$1=br-extra" ;; guest|alias) eval "$1=br-guest" ;; iot) eval "$1=br-iot" ;; esac
		}
		# The real ruleset is rendered; only loading it is stubbed.
		cpxy_nft_apply() { [ ! -f /tmp/nft-fails ] || return 1; cp "$1" /tmp/applied.nft; echo "nft_apply" >>"$LOG"; }
		cpxy_nft_clear() { rm -f /tmp/applied.nft; echo "nft_clear" >>"$LOG"; }
		cpxy_nft_main_routes() { echo 198.18.0.0/15; }
		# /tmp/no-fw4 stands for fw3 firmware (OpenWrt 21.02 and older): no fw4, no nft;
		# /tmp/fw3-tools for its ipset and iptables-restore. The iptables rules and the ipset
		# input are rendered for real; only loading them is stubbed.
		_cpxy_have_fw4() { [ ! -f /tmp/no-fw4 ]; }
		_cpxy_fw3_firmware() { [ -f /tmp/no-fw4 ]; }
		cpxy_ipt_missing() { [ -f /tmp/fw3-tools ] || echo ipset; }
		cpxy_ipt_apply() { rm -rf /tmp/applied.ipt; cp -R "$1" /tmp/applied.ipt; echo "ipt_apply" >>"$LOG"; }
		cpxy_ipt_clear() { rm -rf /tmp/applied.ipt; echo "ipt_clear" >>"$LOG"; }
		cpxy_ipt_exempt_routes() { echo "exempt_routes $*" >>"$LOG"; echo 100.101.149.52; }
		cpxy_legacy_down() { echo "legacy_down $*" >>"$LOG"; }
		# The uplinks it is given are checked; the router's devices are those of the stubs.
		cpxy_nft_exempt() { echo "exempt_args $*" >>"$LOG"; echo tailscale0; echo br-lan; }
	fi
	"$2"
)
everything() {
	: >"$LOG"
	for s in cpxy-client cpxy-router; do service "$s" "$1" 2>>/tmp/err || fail "$s $1"; done
}
router() { service cpxy-router "$1" 2>>/tmp/err || fail "cpxy-router $1"; }
# invalid <error>: every service refuses the configuration and changes nothing
invalid() {
	: >/tmp/err
	for s in cpxy-client cpxy-router; do
		: >"$LOG"
		if service "$s" reload_service 2>>/tmp/err; then fail "$s accepted an invalid configuration"; fi
		[ ! -s "$LOG" ] || fail "$s: invalid reload changed the running service"
	done
	grep -qF -- "$1" /tmp/err || fail "missing error: $1"
}
reload_service() { start_service; }

# The default configuration waits for a server
invalid "main: option 'server' is not set"

uci set cpxy.defaults.server='http://:test-key@proxy.example:8080'
uci add_list cpxy.main.network=extra
uci set cpxy.guest=instance
uci set cpxy.guest.index=1
uci add_list cpxy.guest.dns_search_domain=home.arpa
uci add_list cpxy.guest.dns_search_domain=corp.example
uci add_list cpxy.guest.network=guest
uci set cpxy.guest.server='http://:guest-key@second.example:8080'
uci set cpxy.iot=instance
uci set cpxy.iot.index=2
uci add_list cpxy.iot.network=iot
uci set cpxy.iot.client_socks5=1
uci set cpxy.iot.dns_cache=0
uci commit cpxy

everything start_service
# cpxy-client: the HTTP proxy, the SOCKS5 listener the DNS worker uses, and the transparent
# listener the networks' TCP is redirected to
want 'set command /usr/bin/cpxy client'
want 'set env SERVER=http://:test-key@proxy.example:8080 HTTP_PROXY_LISTEN=[::]:5000 NO_COLOR=1 SOCKS5_PROXY_LISTEN=[::]:1080 TRANSPARENT_PROXY_LISTEN=0.0.0.0:5100'
want 'set env SERVER=http://:guest-key@second.example:8080 HTTP_PROXY_LISTEN=[::]:5001 NO_COLOR=1 SOCKS5_PROXY_LISTEN=[::]:1081 TRANSPARENT_PROXY_LISTEN=0.0.0.0:5101'
want 'HTTP_PROXY_LISTEN=[::]:5002 NO_COLOR=1 SOCKS5_PROXY_LISTEN=[::]:1082 TRANSPARENT_PROXY_LISTEN=0.0.0.0:5102'
# Without routes, PROXY_ROUTES is left unset: cpxy-client rejects an empty one
absent 'PROXY_ROUTES'
# procd restarts an instance when this file changes, not when only its environment does
want 'set file /tmp/cpxy/client-guest.env'
grep -qxF 'SERVER=http://:guest-key@second.example:8080' /tmp/cpxy/client-guest.env || fail 'client settings file'
[ "$(ls -l /tmp/cpxy/client-guest.env | cut -c1-10)" = '-rw-------' ] || fail 'settings file readable by others'
# cpxy-router: one DNS worker per instance, no TUN
want 'instance guest_dns'
want 'set command /usr/bin/cpxy dns --resolver-routing cn_split --socks5-proxy 127.0.0.1:1081'
absent '--tun'
want 'set env NO_COLOR=1'
want 'append command --listen [::]:5345'
want 'append command --alternative tcp://1.1.1.1?via=cpxy'
absent 'append command --alternative tcp://8.8.8.8'
# The worker asks those through its instance's client, whose key stays out of the process list
if grep -q SERVER /tmp/cpxy/router-guest.env; then fail 'router has cpxy credentials'; fi
absent 'append command --server'
want 'append command --local-server 127.0.0.1:53'
want 'append command --local-domain lan'
want 'append command --search-domain home.arpa'
want 'append command --search-domain corp.example'
[ "$(grep -c -- '--cache off' "$LOG")" = 1 ] || fail 'dns_cache off for iot only'
[ "$(grep -c -- '--local-domain lan' "$LOG")" = 3 ] || fail 'local domains repeated'
want 'set file /tmp/cpxy/router-guest.env'
want 'append file /usr/bin/cpxy'
# The kernel policy: one table, per instance a direct set, redirects and guards per device
want 'nft_apply'
nft_has 'table inet cpxy {'
nft_has 'set direct_main {'
nft_has '198.18.0.0/15'
nft_has 'iifname "br-lan" meta nfproto ipv4 tcp dport != 53 fib daddr type != { local, broadcast, multicast } ip daddr != @direct_main fib daddr oif != @exempt counter redirect to :5100'
# Routes through other devices (tailscale0) win over the proxy; uplinks are never exempt
nft_has 'elements = { "tailscale0", "br-lan" }'
nft_has 'ip daddr != @direct_guest oif != @exempt counter reject with tcp reset'
grep 'exempt_args' "$LOG" | grep -qw wwan9 || fail 'masquerading zone device not an uplink'
nft_has 'iifname "br-extra" meta nfproto ipv4 tcp dport != 53'
nft_has 'ip daddr != @direct_guest fib daddr oif != @exempt counter redirect to :5101'
nft_has 'ip daddr != @direct_iot fib daddr oif != @exempt counter redirect to :5102'
nft_has 'iifname "br-guest" meta nfproto ipv4 meta l4proto tcp ip daddr != @direct_guest oif != @exempt counter reject with tcp reset comment "cpxy guard br-guest"'
nft_has 'iifname "br-iot" meta nfproto ipv4 udp dport 443 counter reject'
nft_has 'iifname "br-lan" meta nfproto ipv6 counter reject'
[ "$(head -n 1 /etc/cpxy/router-guards/main)" = nft ] || fail 'boot snapshot format'
[ "$(sed -n 2p /etc/cpxy/router-guards/main)" = 'br-lan br-extra' ] || fail 'boot snapshot devices'
# The firewall: the listener reachable from each source zone, and DNS redirects
[ "$(uci get firewall.cpxy_inst_main_proxy1.src)" = trusted ] || fail 'source zone lookup'
[ "$(uci get firewall.cpxy_inst_main_proxy1.dest_port)" = 5100 ] || fail 'listener port rule'
[ "$(uci get firewall.cpxy_inst_guest_proxy1.target)" = ACCEPT ] || fail 'listener rule target'
[ "$(uci get firewall.cpxy_inst_guest_proxy1.family)" = ipv4 ] || fail 'listener rule family'
if uci -q get firewall.cpxy_inst_guest_zone; then fail 'TUN zone remains'; fi
[ "$(uci get firewall.cpxy_inst_guest_dns1.src)" = visitors ] || fail 'DNS redirect zone'
[ "$(uci get firewall.cpxy_inst_guest_dns1.dest_port)" = 5345 ] || fail 'DNS redirect port'
[ "$(uci get firewall.cpxy_inst_guest_dns1.target)" = DNAT ] || fail 'DNS redirect target'
[ "$(uci get firewall.cpxy_inst_iot_dns6_1.target)" = DNAT ] || fail 'IPv6 DNS redirect'
[ "$(uci get firewall.cpxy_inst_iot_dns6_1.family)" = ipv6 ] || fail 'IPv6 DNS redirect family'
[ "$(uci get firewall.cpxy_boot_guard.path)" = /usr/libexec/cpxy/boot-firewall.sh ] || fail 'missing boot firewall include'

# Fixed routes inherit defaults, while an instance's list replaces them.
uci add_list cpxy.defaults.proxy_route=223.5.5.5/32
uci add_list cpxy.defaults.proxy_route=10.20.0.0/16
uci add_list cpxy.guest.proxy_route=192.0.2.0/24
uci add_list cpxy.guest.proxy_route=198.51.100.0/24
uci add_list cpxy.guest.proxy_route=2400:3200::/32
everything reload_service
want 'HTTP_PROXY_LISTEN=[::]:5000 NO_COLOR=1 PROXY_ROUTES=223.5.5.5/32,10.20.0.0/16'
want 'HTTP_PROXY_LISTEN=[::]:5001 NO_COLOR=1 PROXY_ROUTES=192.0.2.0/24,198.51.100.0/24,2400:3200::/32'
want 'HTTP_PROXY_LISTEN=[::]:5002 NO_COLOR=1 PROXY_ROUTES=223.5.5.5/32,10.20.0.0/16'
for route in 223.5.5.5/32 10.20.0.0/16; do
	[ "$(grep -cF -- "append command --proxy-route $route" "$LOG")" = 2 ] || fail 'default routes not inherited by main and iot'
done
for route in 192.0.2.0/24 198.51.100.0/24 2400:3200::/32; do
	[ "$(grep -cF -- "append command --proxy-route $route" "$LOG")" = 1 ] || fail 'guest routes not isolated to guest'
done
grep -qxF 'PROXY_ROUTES=223.5.5.5/32,10.20.0.0/16' /tmp/cpxy/client-main.env || fail 'client default routes'
grep -qxF 'PROXY_ROUTES=192.0.2.0/24,198.51.100.0/24,2400:3200::/32' /tmp/cpxy/client-guest.env || fail 'client instance routes'
absent 'firewall reload'
absent 'legacy_down'

# Invalid route reloads preserve the running services and kernel configuration.
for route in 10.0.0.0/33 256.0.0.0/8 1.2.3.4 example.com/24 1::2::3/64 2001:db8::/129 ::1; do
	uci set cpxy.guest.proxy_route="$route"
	invalid "invalid proxy_route '$route'"
done

# Removing both lists clears the worker arguments and persisted proxy environments.
uci delete cpxy.defaults.proxy_route
uci delete cpxy.guest.proxy_route
everything reload_service
absent 'append command --proxy-route'
absent 'PROXY_ROUTES'
if grep -q '^PROXY_ROUTES' /tmp/cpxy/client-main.env; then fail 'client routes not cleared'; fi

# Obsolete options are ignored with a log line; the instance keeps running.
uci set cpxy.guest.tun=cpxy7
uci set cpxy.guest.routing_table=150
uci set cpxy.guest.rule_priority=9300
uci set cpxy.guest.ipv6=on
uci set cpxy.guest.router_ipv6=full
: >/tmp/err
: >"$LOG"
router reload_service
grep -qF "guest: option 'tun' is obsolete and ignored" /tmp/err || fail 'obsolete tun not reported'
grep -qF "guest: option 'rule_priority' is obsolete and ignored" /tmp/err || fail 'obsolete rule_priority not reported'
want 'instance guest_dns'
absent 'cpxy7'
nft_lacks 'cpxy7'
for option in tun routing_table rule_priority ipv6 router_ipv6; do uci delete "cpxy.guest.$option"; done
router reload_service

# Idempotent reload keeps the firewall unchanged.
: >"$LOG"
router reload_service
absent 'firewall reload'
[ -z "$(uci -q changes firewall)" ] || fail 'idempotent reload staged firewall changes'

# A failed table load changes nothing else and keeps the old policy.
cp /tmp/applied.nft /tmp/applied.before
touch /tmp/nft-fails
uci set cpxy.guest.client_transparent_listen=0.0.0.0:5200
: >"$LOG"
if service cpxy-router reload_service 2>/tmp/err; then fail 'reload succeeded without its table'; fi
rm /tmp/nft-fails
cmp -s /tmp/applied.nft /tmp/applied.before || fail 'failed load replaced the policy'
absent 'firewall reload'
absent 'instance '
[ "$(uci get firewall.cpxy_inst_guest_proxy1.dest_port)" = 5101 ] || fail 'failed load changed the firewall'
uci delete cpxy.guest.client_transparent_listen

# Conflicts between instances
uci set cpxy.guest.index=0
invalid 'index 0 is already used'
uci set cpxy.guest.index=1
uci set cpxy.guest.client_http_listen='[::]:5000'
invalid 'tcp 5000 is already used'
uci delete cpxy.guest.client_http_listen
uci set cpxy.guest.client_transparent_listen=0.0.0.0:5100
invalid 'tcp 5100 is already used'
uci set cpxy.guest.client_transparent_listen='[::]:5101'
invalid 'client_transparent_listen must bind 0.0.0.0:port'
uci delete cpxy.guest.client_transparent_listen
uci set cpxy.guest.dns_listen=0.0.0.0:53
invalid 'must not use port 53'
uci delete cpxy.guest.dns_listen
uci add_list cpxy.guest.dns_alternative='udp://8.8.8.8?via=cpxy'
invalid "DNS server 'udp://8.8.8.8?via=cpxy' goes through the cpxy server"
uci delete cpxy.guest.dns_alternative
uci add_list cpxy.guest.dns_alternative='8.8.8.8?via=cpxy'
invalid "DNS server '8.8.8.8?via=cpxy' goes through the cpxy server"
uci delete cpxy.guest.dns_alternative
# Routed TCP needs the instance's own client
uci set cpxy.guest.router_socks5_proxy='[2001:db8::1]:1080'
invalid 'router_socks5_proxy is no longer supported'
uci delete cpxy.guest.router_socks5_proxy
uci set cpxy.guest.client=0
invalid "guest: a routed network needs client '1'"
uci set cpxy.guest.client=1
uci set cpxy.main.router_geoip=0
invalid 'router_geoip must be 1'
uci set cpxy.main.router_geoip=1
# Settings of the smartdns this package no longer has are ignored
uci set cpxy.guest.smartdns=1
uci add_list cpxy.guest.smartdns_server=tcp://1.1.1.1
uci commit cpxy
router reload_service
uci delete cpxy.guest.smartdns
uci delete cpxy.guest.smartdns_server
uci delete cpxy.main.network
uci add_list cpxy.main.network=lan
: >/tmp/err
if service cpxy-router reload_service 2>/tmp/err; then fail 'zone shared with an unmanaged network accepted'; fi
grep -qF "also holds network 'extra'" /tmp/err || fail 'missing error: zone holds another network'
# Without router DNS the table still redirects the networks' TCP, and no worker runs.
uci set cpxy.main.router_dns=0
: >"$LOG"
router reload_service
absent 'instance main_dns'
want 'instance guest_dns'
nft_has 'iifname "br-lan" meta nfproto ipv4 tcp dport != 53'
nft_lacks 'br-extra'
if uci -q get firewall.cpxy_inst_main_dns1; then fail 'router_dns 0 still redirects DNS'; fi
[ "$(uci get firewall.cpxy_inst_main_proxy1.dest_port)" = 5100 ] || fail 'router_dns 0 lost the listener rule'
uci set cpxy.main.router_dns=1
uci add_list cpxy.main.network=extra
uci add_list cpxy.guest.network=extra
: >/tmp/err
if service cpxy-router reload_service 2>/tmp/err; then fail 'network in two instances accepted'; fi
grep -qF "network 'extra' is assigned more than once" /tmp/err || fail 'missing error: network twice'
uci delete cpxy.guest.network
uci add_list cpxy.guest.network=guest
uci set firewall.aliases=zone
uci set firewall.aliases.name=aliases
uci set firewall.aliases.network=alias
uci add_list cpxy.iot.network=alias
: >/tmp/err
if service cpxy-router reload_service 2>/tmp/err; then fail 'device in two instances accepted'; fi
grep -qF "device 'br-guest' is assigned more than once" /tmp/err || fail 'missing error: device twice'
uci delete cpxy.iot.network
uci add_list cpxy.iot.network=iot
uci delete firewall.aliases
router reload_service
uci set firewall.@defaults[0].flow_offloading=1
if service cpxy-router reload_service 2>/tmp/err; then fail 'flow offloading accepted'; fi
grep -q 'disable flow_offloading' /tmp/err || fail 'missing offloading error'
uci set firewall.@defaults[0].flow_offloading=0
touch /tmp/no-lan
if service cpxy-router reload_service 2>/tmp/err; then fail 'activated with an unresolved LAN'; fi
grep -q 'retaining boot protection' /tmp/err || fail 'missing unresolved LAN error'
rm /tmp/no-lan

# The proxies send everything through the server unless cn_split is asked for
absent 'ROUTING='
uci set cpxy.guest.client_routing=cn_split
uci commit cpxy
: >"$LOG"
service cpxy-client start_service 2>>/tmp/err || fail 'cn_split client'
want 'set env SERVER=http://:guest-key@second.example:8080 HTTP_PROXY_LISTEN=[::]:5001 NO_COLOR=1 ROUTING=cn_split DNS_SERVER=127.0.0.1 SOCKS5_PROXY_LISTEN=[::]:1081'
[ "$(grep -c 'ROUTING=cn_split' "$LOG")" = 1 ] || fail 'cn_split for guest only'
uci add_list cpxy.guest.client_dns_server=223.5.5.5
uci add_list cpxy.guest.client_dns_server='[2001:db8::53]:5353'
uci commit cpxy
: >"$LOG"
service cpxy-client start_service 2>>/tmp/err || fail 'cn_split client with resolvers'
want 'ROUTING=cn_split DNS_SERVER=223.5.5.5,[2001:db8::53]:5353 '
for server in 223.5.5.5:0 dns.example 300.1.1.1 '2001:db8::53::1' '[2001:db8::53]'; do
	uci delete cpxy.guest.client_dns_server
	uci add_list cpxy.guest.client_dns_server="$server"
	uci commit cpxy
	invalid "invalid client_dns_server '$server'"
done
uci delete cpxy.guest.client_dns_server
uci set cpxy.guest.client_routing=cn
uci commit cpxy
invalid 'client_routing must be global or cn_split'
uci delete cpxy.guest.client_routing
uci commit cpxy

# Disable guest without touching the others
uci set cpxy.guest.enabled=0
everything reload_service
absent 'instance guest'
want 'instance main_dns'
nft_lacks 'br-guest'
nft_lacks 'direct_guest'
nft_has 'cpxy guard br-lan'
[ ! -f /etc/cpxy/router-guards/guest ] || fail 'disabled guest keeps its boot snapshot'
if uci -q get firewall.cpxy_inst_guest_proxy1; then fail 'disabled guest listener rule remains'; fi
if uci -q get firewall.cpxy_inst_guest_dns1; then fail 'disabled guest DNS redirect remains'; fi
[ "$(uci get firewall.cpxy_inst_main_proxy1.src)" = trusted ] || fail 'main listener rule removed'
# Interface triggers for every routed network
procd_add_reload_trigger() { :; }
procd_add_interface_trigger() { echo "trigger $*" >>"$LOG"; }
procd_add_raw_trigger() { echo "raw_trigger $*" >>"$LOG"; }
: >"$LOG"
router service_triggers
want 'trigger interface.* lan /etc/init.d/cpxy-router reload'
want 'trigger interface.* iot /etc/init.d/cpxy-router reload'
want 'raw_trigger interface.* 1000 /etc/init.d/cpxy-router reload'
absent 'trigger interface.* guest'

: >"$LOG"
router stop_service
want 'nft_clear'
want 'boot_fw clear'
[ ! -d /tmp/cpxy/instances ] || fail 'runtime state remains'
[ -f /tmp/cpxy/router-stopped ] || fail 'stop marker missing'
if uci show firewall | grep -q cpxy_inst_; then fail 'firewall sections remain after stop'; fi
[ -f /etc/cpxy/router-guards/main ] || fail 'stop lost next-boot assignments'

# Upgrade from the TUN design: the new table goes in first, then the old instance's device,
# tables and rules are removed. Its boot snapshot is rewritten in the new format.
mkdir -p /tmp/cpxy/instances
printf '%s\n' cpxy0 100 9100 'br-lan br-extra' trusted 5335 off 1 '192.0.2.1 wan' abc >/tmp/cpxy/instances/main
printf '%s\n' cpxy1 101 9110 br-guest visitors 5345 off 1 >/etc/cpxy/router-guards/guest
printf '%s\n' cpxy0 100 9100 'br-lan br-extra' trusted 5335 off 1 >/etc/cpxy/router-guards/main
: >"$LOG"
router start_service
want 'legacy_down cpxy0 100 9100'
[ "$(grep -n nft_apply "$LOG" | cut -d: -f1)" -lt "$(grep -n legacy_down "$LOG" | cut -d: -f1)" ] || fail 'legacy state removed before the new guards'
[ "$(head -n 1 /etc/cpxy/router-guards/main)" = nft ] || fail 'TUN-era boot snapshot kept'
[ ! -f /etc/cpxy/router-guards/guest ] || fail 'disabled instance keeps its TUN-era snapshot'
[ "$(head -n 1 /tmp/cpxy/instances/main)" = nft ] || fail 'state not migrated'
: >"$LOG"
router reload_service
absent 'legacy_down'
# Stopping during an upgrade also removes TUN-era kernel state
printf '%s\n' cpxy2 102 9120 br-iot things 5355 off 1 >/tmp/cpxy/instances/iot
: >"$LOG"
router stop_service
want 'legacy_down cpxy2 102 9120'

# Instances without routed networks: client only, no transparent or SOCKS5 listener.
uci set cpxy.main.enabled=0
uci set cpxy.iot.enabled=0
uci set cpxy.guest.enabled=1
uci delete cpxy.guest.network
uci set cpxy.guest.dns_listen=0.0.0.0:5345
: >"$LOG"
everything start_service
want 'set command /usr/bin/cpxy client'
absent 'set command /usr/bin/cpxy dns'
absent 'SOCKS5_PROXY_LISTEN='
absent 'TRANSPARENT_PROXY_LISTEN='
want 'nft_apply'
nft_lacks 'table inet cpxy {'

# fw3 firmware: the package installs without fw4, and a client-only instance runs; nothing
# touches nftables. Without ipset and iptables-restore a routed network is refused with a reason,
# and the client still starts; with them it is routed with iptables.
touch /tmp/no-fw4
: >"$LOG"
everything start_service
want 'set command /usr/bin/cpxy client'
absent 'nft_apply'
uci add_list cpxy.guest.network=guest
uci set cpxy.guest.dns_listen='[::]:5345'
uci commit cpxy
: >"$LOG"
service cpxy-client start_service 2>>/tmp/err || fail 'client refused on fw3 firmware'
want 'set command /usr/bin/cpxy client'
: >"$LOG"
if service cpxy-router start_service 2>/tmp/err; then fail 'routed network accepted without ipset'; fi
grep -qF "guest: routing networks on fw3 firmware needs ipset" /tmp/err || fail 'missing error: routing needs ipset'
absent 'nft_apply'
absent 'ipt_apply'
touch /tmp/fw3-tools
uci set cpxy.main.enabled=1
uci commit cpxy
: >"$LOG"
router start_service
absent 'nft_apply'
want 'ipt_apply'
want 'exempt_routes '
ipt_has() { grep -qF -- "$2" "/tmp/applied.ipt/$1" || { cat "/tmp/applied.ipt/$1" | grep -v '^add '; fail "$1 lacks: $2"; }; }
ipt_lacks() { if grep -qF -- "$2" "/tmp/applied.ipt/$1"; then fail "$1 has: $2"; fi; }
ipt_has sets cpxy_main
ipt_has sets cpxy_guest
ipt_has ipset 'create cpxy_main_n hash:net family inet'
ipt_has ipset 'add cpxy_main_n 198.18.0.0/15'
ipt_has ipset 'add cpxy_guest_n 100.101.149.52'
ipt_has ipt4 '-A cpxy_prerouting -i br-lan -p udp --dport 53 -m comment --comment "cpxy dns br-lan" -j REDIRECT --to-ports 5335'
ipt_has ipt4 '-A cpxy_prerouting -i br-lan -p tcp -m addrtype ! --dst-type LOCAL -m set ! --match-set cpxy_main dst -m comment --comment "cpxy redirect br-lan" -j REDIRECT --to-ports 5100'
ipt_has ipt4 '-A cpxy_prerouting -i br-guest -p tcp -m addrtype ! --dst-type LOCAL -m set ! --match-set cpxy_guest dst -m comment --comment "cpxy redirect br-guest" -j REDIRECT --to-ports 5101'
ipt_has ipt4 '-A cpxy_forward -i br-lan -p tcp -m set ! --match-set cpxy_main dst -m comment --comment "cpxy guard br-lan" -j REJECT --reject-with tcp-reset'
ipt_has ipt4 '-A cpxy_forward -i br-guest -p udp --dport 443'
ipt_has ipt4 '-A cpxy_input -i br-guest -p udp --dport 5345 -m conntrack --ctstate DNAT -j ACCEPT'
ipt_lacks ipt4 'cpxy ipv6'
# fw3 cannot DNAT IPv6: DNS is redirected in the rules, IPv6 forwarding refused
ipt_has ipt6 '-A cpxy_prerouting -i br-guest -p tcp --dport 53 -m comment --comment "cpxy dns br-guest" -j REDIRECT --to-ports 5345'
ipt_has ipt6 '-A cpxy_forward -i br-lan -m comment --comment "cpxy ipv6 br-lan" -j REJECT'
ipt_lacks ipt6 'cpxy redirect'
ipt_lacks ipt6 '--dport 5100'
# The firewall keeps the listener rules but no DNS redirect sections
[ "$(uci get firewall.cpxy_inst_main_proxy1.src)" = trusted ] || fail 'main listener rule on fw3'
if uci show firewall | grep -q 'cpxy_inst_.*_dns'; then fail 'fw3 got DNS redirect sections'; fi
: >"$LOG"
router stop_service
want 'ipt_clear'
rm /tmp/no-fw4 /tmp/fw3-tools
echo 'multi-instance init and firewall checks passed'
SCRIPT
)"
"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
