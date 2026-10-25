#!/bin/sh
# Exercise the three init scripts with three instances, and real UCI firewall generation, in an
# OpenWrt rootfs. procd, ip and the routing helpers are stubbed: commands are logged, not run.
#
# usage: init-dryrun.sh <ipk> [image]    (needs podman or docker)
set -eu
ipk="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
image="${2:-docker.io/openwrt/rootfs:x86-64-openwrt-24.10}"
engine="$(command -v podman || command -v docker)"
script="$(cat <<'SCRIPT'
set -e
mkdir -p /var/lock /tmp
for dep in ip-full kmod-tun; do
	printf 'Package: %s\nVersion: 1\nStatus: install ok installed\nArchitecture: x86_64\n\n' "$dep" >>/usr/lib/opkg/status
	: >"/usr/lib/opkg/info/$dep.list"
done
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
mkdir /tmp/tuns
ip() {
	case "$1 $2" in
		'link show') [ -f "/tmp/tuns/$3" ] ;;
		'tuntap show') echo "$4: tun" ;;
		'-4 route') echo "route $*" >>"$LOG" ;;
		*) command ip "$@" ;;
	esac
}
fail() { echo "FAIL: $*"; cat "$LOG"; cat /tmp/err 2>/dev/null; exit 1; }
want() { grep -qF -- "$1" "$LOG" || fail "expected: $1"; }
absent() { if grep -qF -- "$1" "$LOG"; then fail "unexpected: $1"; fi; }
# service <name> <function>: runs one init script's function in a shell of its own
service() (
	. "/etc/init.d/$1"
	if [ "$1" = cpxy-router ]; then
		# 'alias' is another network on the guest bridge
		network_get_device() { case "$2" in lan) eval "$1=br-lan" ;; extra) eval "$1=br-extra" ;; guest|alias) eval "$1=br-guest" ;; iot) eval "$1=br-iot" ;; esac; }
		cpxy_net_up() { touch "/tmp/tuns/$CPXY_TUN"; echo "up $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN $*" >>"$LOG"; }
		cpxy_net_down_routing() { rm -f "/tmp/tuns/$CPXY_TUN"; echo "down $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN" >>"$LOG"; }
		_cpxy_net_down_rules() { echo "rules_down $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN" >>"$LOG"; }
		cpxy_geo_uplink() { echo '192.0.2.1 wan'; }
		cpxy_geo_guard() { echo "guard $*" >>"$LOG"; }
		cpxy_geo_healthy() { return 0; }
		cpxy_geo_up() { touch "/tmp/tuns/$CPXY_TUN"; echo "geo_up $CPXY_TUN $CPXY_TABLE" >>"$LOG"; }
		cpxy_geo_down() { rm -f "/tmp/tuns/$CPXY_TUN"; echo "geo_down $CPXY_TUN" >>"$LOG"; }
	elif [ "$1" = cpxy-route-guard ]; then
		cpxy_geo_guard() { echo "boot_guard $*" >>"$LOG"; }
	fi
	"$2"
)
everything() {
	: >"$LOG"
	for s in cpxy-client cpxy-router cpxy-tsgw; do service "$s" "$1" 2>>/tmp/err || fail "$s $1"; done
}
router() { service cpxy-router "$1" 2>>/tmp/err || fail "cpxy-router $1"; }
# invalid <error>: every service refuses the configuration and changes nothing
invalid() {
	: >/tmp/err
	for s in cpxy-client cpxy-router cpxy-tsgw; do
		: >"$LOG"
		if service "$s" reload_service 2>>/tmp/err; then fail "$s accepted an invalid configuration"; fi
		[ ! -s "$LOG" ] || fail "$s: invalid reload changed the running service"
	done
	grep -qF -- "$1" /tmp/err || fail "missing error: $1"
}
reload_service() { start_service; }

# The default configuration waits for a server
invalid "main: option 'server' is not set"

uci set cpxy.defaults.server='https://:test-key@proxy.example:443'
uci add_list cpxy.main.network=extra
uci set cpxy.guest=instance
uci set cpxy.guest.index=1
uci add_list cpxy.guest.dns_search_domain=home.arpa
uci add_list cpxy.guest.dns_search_domain=corp.example
uci add_list cpxy.guest.network=guest
uci set cpxy.guest.server='https://:guest-key@second.example:443'
uci set cpxy.iot=instance
uci set cpxy.iot.index=2
uci add_list cpxy.iot.network=iot
uci set cpxy.iot.tsgw=0
uci set cpxy.iot.client_socks5=1
uci set cpxy.iot.dns_cache=0
uci commit cpxy

everything start_service
# cpxy-client
want 'set command /usr/bin/cpxy-client'
want 'set env CONFIG=https://:test-key@proxy.example:443 HTTP_PROXY_LISTEN=[::]:5000 NO_COLOR=1'
want 'set env CONFIG=https://:guest-key@second.example:443 HTTP_PROXY_LISTEN=[::]:5001 NO_COLOR=1'
want 'HTTP_PROXY_LISTEN=[::]:5002 NO_COLOR=1 SOCKS5_PROXY_LISTEN=[::]:1082'
# Without routes, PROXY_ROUTES is left unset: cpxy-client rejects an empty one
absent 'PROXY_ROUTES'
# procd restarts an instance when this file changes, not when only its environment does
want 'set file /tmp/cpxy/client-guest.env'
grep -qxF 'CONFIG=https://:guest-key@second.example:443' /tmp/cpxy/client-guest.env || fail 'client settings file'
[ "$(ls -l /tmp/cpxy/client-guest.env | cut -c1-10)" = '-rw-------' ] || fail 'settings file readable by others'
# cpxy-router: one worker per instance, which also answers its DNS
want 'up cpxy0 100 9100 br-lan'
want 'up cpxy1 101 9110 br-guest'
want 'up cpxy2 102 9120 br-iot'
want 'set command /usr/bin/cpxy-router --tun cpxy1 --ipv6 full'
want 'set env SERVER=https://:guest-key@second.example:443 NO_COLOR=1'
absent '_dns'
absent 'cpxy-dns-split'
want 'append command --dns-listen 0.0.0.0:5345'
want 'append command --dns-alternative tcp://1.1.1.1?via=cpxy'
absent 'append command --dns-alternative tcp://8.8.8.8'
# The worker asks those through its instance's server, whose key stays out of the process list
grep -qxF 'SERVER=https://:guest-key@second.example:443' /tmp/cpxy/router-guest.env || fail 'router server'
absent 'append command --server'
want 'append command --dns-local-server 127.0.0.1:53'
want 'append command --dns-local-domain lan'
want 'append command --dns-search-domain home.arpa'
want 'append command --dns-search-domain corp.example'
grep -qxF 'DNS_SEARCH_DOMAINS=home.arpa,corp.example' /tmp/cpxy/tsgw-guest.env || fail 'tsgw search domains'
[ "$(grep -c -- '--no-dns-cache' "$LOG")" = 1 ] || fail 'dns_cache off for iot only'
[ "$(grep -c -- '--dns-local-domain lan' "$LOG")" = 3 ] || fail 'local domains repeated'
absent 'append command --sinkhole'
[ "$(uci get firewall.cpxy_inst_main_fwd1.src)" = trusted ] || fail 'source zone lookup'
[ "$(uci get firewall.cpxy_inst_guest_zone.device)" = cpxy1 ] || fail 'TUN zone'
[ "$(uci get firewall.cpxy_inst_guest_udp.target)" = REJECT ] || fail 'UDP rejection'
[ "$(uci get firewall.cpxy_inst_guest_dns1.src)" = visitors ] || fail 'DNS redirect zone'
[ "$(uci get firewall.cpxy_inst_guest_dns1.dest_port)" = 5345 ] || fail 'DNS redirect port'
[ "$(uci get firewall.cpxy_inst_guest_dns1.target)" = DNAT ] || fail 'DNS redirect target'
[ "$(uci get firewall.cpxy_inst_iot_dns6_1.target)" = REJECT ] || fail 'IPv6 DNS refusal'
[ "$(uci get firewall.cpxy_inst_iot_dns6f_1.dest)" = '*' ] || fail 'forwarded IPv6 DNS refusal'
position() { uci show firewall | grep -E '^firewall\.[^.]+=' | grep -n "^firewall\.$1=" | cut -d: -f1; }
[ "$(position cpxy_inst_guest_dns6_1)" -lt "$(position visitors_dns)" ] || fail 'IPv6 DNS refusal after an accept rule'
[ "$(position cpxy_inst_guest_dns6f_1)" -lt "$(position visitors_dns)" ] || fail 'forwarded IPv6 DNS refusal after an accept rule'
# cpxy-tsgw: main and guest only
want 'set command /usr/bin/cpxy-tsgw'
want 'TS_HOSTNAME=cpxy-guest TS_PORT=41701'
want 'DNS_UPSTREAM=223.5.5.5 DNS_ALTERNATIVE=tcp://1.1.1.1?via=cpxy IPV6=full'
want 'DNS_SEARCH_DOMAINS=home.arpa,corp.example HTTPS_PROXY=http://127.0.0.1:5001 HTTP_PROXY=http://127.0.0.1:5001'
want 'set file /tmp/cpxy/tsgw-guest.env'
grep -qxF 'IPV6=full' /tmp/cpxy/tsgw-guest.env || fail 'tsgw IPv6 mode'
want 'set file /tmp/cpxy/router-guest.env'
absent 'TS_HOSTNAME=cpxy-iot'
[ -d /etc/cpxy/tsgw/guest ] || fail 'tsgw state directory'
absent 'TS_AUTHKEY'

# Monitoring is opt-in, uses one port per instance, and never changes routing on reload.
absent 'append command --api-listen'
uci set cpxy.defaults.router_api=1
uci set cpxy.iot.router_api=0
uci set cpxy.guest.router_api_listen='[::1]:3021'
everything reload_service
want 'append command --api-listen 127.0.0.1:3010'
want 'append command --api-listen [::1]:3021'
[ "$(grep -c -- '--api-listen' "$LOG")" = 2 ] || fail 'API enable override'
absent 'up '
absent 'down '
uci set cpxy.guest.router_api_listen=127.0.0.1:5000
invalid 'tcp 5000 is already used'
for address in bad 127.0.0.1:0 127.0.0.1:65536 999.0.0.1:3011 ::1:3011; do
	uci set cpxy.guest.router_api_listen="$address"
	invalid 'router_api_listen must be an IPv4:port or [IPv6]:port address'
done
uci delete cpxy.guest.router_api_listen
everything reload_service
want 'append command --api-listen 127.0.0.1:3011'
uci delete cpxy.defaults.router_api
uci delete cpxy.iot.router_api
everything reload_service
absent 'append command --api-listen'

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
grep -qxF 'PROXY_ROUTES=223.5.5.5/32,10.20.0.0/16' /tmp/cpxy/tsgw-main.env || fail 'tsgw default routes'
grep -qxF 'PROXY_ROUTES=192.0.2.0/24,198.51.100.0/24,2400:3200::/32' /tmp/cpxy/tsgw-guest.env || fail 'tsgw instance routes'
absent 'firewall reload'
absent 'down '
absent 'up '

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
if grep -q '^PROXY_ROUTES' /tmp/cpxy/tsgw-guest.env; then fail 'tsgw routes not cleared'; fi

# Changing an instance's IPv6 mode sets its routing up again and restarts its worker.
uci set cpxy.guest.ipv6=direct
: >"$LOG"
router reload_service
want 'up cpxy1 101 9110 br-guest'
absent 'up cpxy0'
want 'set command /usr/bin/cpxy-router --tun cpxy1 --ipv6 direct'
want 'set command /usr/bin/cpxy-router --tun cpxy0 --ipv6 full'
uci set cpxy.guest.ipv6=on
invalid "guest: option 'ipv6' must be off, direct or full"
uci delete cpxy.guest.ipv6
router reload_service

# Idempotent reload preserves resource ownership and avoids firewall reloads.
: >"$LOG"
router reload_service
absent 'firewall reload'
absent 'down '
absent 'up '
[ -z "$(uci -q changes firewall)" ] || fail 'idempotent reload staged firewall changes'

# Conflicts between instances
uci set cpxy.guest.index=0
invalid 'index 0 is already used'
uci set cpxy.guest.index=1
uci add_list cpxy.guest.dns_search_domain=home.arpa
uci add_list cpxy.guest.dns_search_domain=corp.example
uci set cpxy.guest.client_http_listen='[::]:5000'
invalid 'tcp 5000 is already used'
uci delete cpxy.guest.client_http_listen
uci set cpxy.guest.dns_listen=0.0.0.0:53
invalid 'must not use port 53'
uci delete cpxy.guest.dns_listen
uci add_list cpxy.guest.dns_alternative='udp://8.8.8.8?via=cpxy'
invalid "DNS server 'udp://8.8.8.8?via=cpxy' goes through the cpxy server"
uci delete cpxy.guest.dns_alternative
uci add_list cpxy.guest.dns_alternative='8.8.8.8?via=cpxy'
invalid "DNS server '8.8.8.8?via=cpxy' goes through the cpxy server"
uci delete cpxy.guest.dns_alternative
# Settings of the smartdns this package no longer has are ignored
uci set cpxy.guest.smartdns=1
uci add_list cpxy.guest.smartdns_server=tcp://1.1.1.1
uci commit cpxy
router reload_service
uci delete cpxy.guest.smartdns
uci delete cpxy.guest.smartdns_server
uci set cpxy.guest.tun=cpxy0
: >/tmp/err
if service cpxy-router reload_service 2>/tmp/err; then fail 'duplicate tun accepted'; fi
grep -qF 'duplicate tun' /tmp/err || fail 'missing error: duplicate tun'
uci delete cpxy.guest.tun
uci delete cpxy.main.network
uci add_list cpxy.main.network=lan
: >/tmp/err
if service cpxy-router reload_service 2>/tmp/err; then fail 'zone shared with an unmanaged network accepted'; fi
grep -qF "also holds network 'extra'" /tmp/err || fail 'missing error: zone holds another network'
uci set cpxy.main.router_dns=0
: >"$LOG"
router reload_service
want 'set command /usr/bin/cpxy-router --tun cpxy0'
[ "$(grep -c -- '--dns-listen' "$LOG")" = 2 ] || fail 'router_dns 0 still answers DNS'
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

# Disable guest without touching the others
uci set cpxy.guest.enabled=0
everything reload_service
want 'down cpxy1 101 9110'
absent 'down cpxy0'
absent 'instance guest'
want 'instance main'
if uci -q get firewall.cpxy_inst_guest_zone; then fail 'disabled guest zone remains'; fi
if uci -q get firewall.cpxy_inst_guest_dns1; then fail 'disabled guest DNS redirect remains'; fi
[ "$(uci get firewall.cpxy_inst_main_fwd1.src)" = trusted ] || fail 'main forwarding removed'
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

router stop_service
[ ! -d /tmp/cpxy/instances ] || fail 'runtime state remains'
if uci show firewall | grep -q cpxy_inst_; then fail 'firewall sections remain after stop'; fi

# Kernel GeoIP is IPv4-only independently of the shared Tailscale IPv6 setting.
uci set cpxy.main.router_geoip=1
: >"$LOG"
router start_service
want 'guard cpxy0 br-lan br-extra'
want 'geo_up cpxy0 100'
want 'set command /usr/bin/cpxy-router --tun cpxy0 --ipv6 off'
want 'append command --proxy-route 0.0.0.0/0'
want 'append file /usr/bin/cpxy-router'
[ "$(uci get firewall.cpxy_boot_guard.path)" = /usr/libexec/cpxy/boot-firewall.sh ] || fail 'missing boot firewall include'
uci set cpxy.main.router_ipv6=full
if service cpxy-router reload_service 2>/tmp/err; then fail 'GeoIP IPv6 accepted'; fi
grep -q 'requires router_ipv6 off' /tmp/err || fail 'missing GeoIP IPv6 error'
uci set cpxy.main.router_ipv6=off
uci set firewall.@defaults[0].flow_offloading=1
if service cpxy-router reload_service 2>/tmp/err; then fail 'GeoIP offloading accepted'; fi
grep -q 'disable flow_offloading' /tmp/err || fail 'missing GeoIP offloading error'
uci set firewall.@defaults[0].flow_offloading=0
uci set cpxy.main.routing_table=110
if service cpxy-router reload_service 2>/tmp/err; then fail 'GeoIP live table reassignment accepted'; fi
grep -q 'stop cpxy-router' /tmp/err || fail 'missing GeoIP migration error'
uci delete cpxy.main.routing_table
: >"$LOG"
router stop_service
want 'geo_down cpxy0'
[ -f /etc/cpxy/router-guards/main ] || fail 'stop lost next-boot assignments'
: >"$LOG"
service cpxy-route-guard start
want 'boot_guard cpxy0 br-lan br-extra'
[ -f /tmp/cpxy/instances/main ] || fail 'boot guard did not record owned resources'
router stop_service
uci set cpxy.main.router_geoip=0
router start_service
[ ! -f /etc/cpxy/router-guards/main ] || fail 'legacy mode retained a stale boot snapshot'
if uci -q get firewall.cpxy_boot_guard; then fail 'legacy mode retained boot firewall include'; fi
router stop_service
echo 'multi-instance init and firewall checks passed'
SCRIPT
)"
"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
