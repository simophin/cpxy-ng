#!/bin/sh
# Exercise the four init scripts with three instances, and real UCI firewall generation, in an
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
. /lib/functions.sh
procd_open_instance() { echo "instance $1" >>"$LOG"; }
procd_close_instance() { :; }
procd_set_param() { echo "set $*" >>"$LOG"; }
procd_append_param() { echo "append $*" >>"$LOG"; }
procd_kill() { echo "FAIL: procd_kill would destroy the current service update"; exit 1; }
rc_procd() { "$@"; }
network_get_device() { case "$2" in lan|extra) eval "$1=br-lan" ;; guest) eval "$1=br-guest" ;; iot) eval "$1=br-iot" ;; esac; }
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
		cpxy_net_up() { touch "/tmp/tuns/$CPXY_TUN"; echo "up $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN $*" >>"$LOG"; }
		cpxy_net_down_routing() { rm -f "/tmp/tuns/$CPXY_TUN"; echo "down $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN" >>"$LOG"; }
		_cpxy_net_down_rules() { echo "rules_down $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN" >>"$LOG"; }
	fi
	"$2"
)
everything() {
	: >"$LOG"
	for s in cpxy-client cpxy-smartdns cpxy-router cpxy-tsgw; do service "$s" "$1" 2>>/tmp/err || return 1; done
}
# invalid <error>: every service refuses the configuration and changes nothing
invalid() {
	: >/tmp/err
	for s in cpxy-client cpxy-smartdns cpxy-router cpxy-tsgw; do
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
uci add_list cpxy.guest.network=guest
uci set cpxy.guest.server='https://:guest-key@second.example:443'
uci set cpxy.iot=instance
uci set cpxy.iot.index=2
uci add_list cpxy.iot.network=iot
uci set cpxy.iot.tsgw=0
uci set cpxy.iot.block_ads=0
uci set cpxy.iot.client_socks5=1
uci commit cpxy

everything start_service
# cpxy-client
want 'set command /usr/bin/cpxy-client'
want 'set env CONFIG=https://:test-key@proxy.example:443 HTTP_PROXY_LISTEN=[::]:5000 NO_COLOR=1'
want 'set env CONFIG=https://:guest-key@second.example:443 HTTP_PROXY_LISTEN=[::]:5001 NO_COLOR=1'
want 'append env SOCKS5_PROXY_LISTEN=[::]:1082'
# cpxy-smartdns: its own port, through its own instance's proxy
want 'set command /usr/bin/cpxy-smartdns -f -c /tmp/cpxy/smartdns-guest.conf -p -'
grep -qxF 'bind 127.0.0.1:5346' /tmp/cpxy/smartdns-guest.conf || fail 'guest smartdns port'
grep -qxF 'proxy-server http://127.0.0.1:5001 -name cpxy' /tmp/cpxy/smartdns-guest.conf || fail 'guest smartdns proxy'
grep -qxF 'server tcp://1.1.1.1 -proxy cpxy' /tmp/cpxy/smartdns-main.conf || fail 'smartdns server'
grep -qxF 'speed-check-mode none' /tmp/cpxy/smartdns-main.conf || fail 'smartdns probes answers'
if grep -q 'resolv\|conf-file\|:53$' /tmp/cpxy/smartdns-main.conf; then fail 'smartdns touches the system resolver'; fi
# cpxy-router: a worker and a dns_split per instance
want 'up cpxy0 100 9100 br-lan'
want 'up cpxy1 101 9110 br-guest'
want 'up cpxy2 102 9120 br-iot'
want 'set command /usr/bin/cpxy-router --tun cpxy1'
want 'set env SERVER=https://:guest-key@second.example:443 NO_COLOR=1'
want 'instance guest_dns'
want 'set command /usr/bin/cpxy-dns-split --listen 0.0.0.0:5345'
want 'append command --alternative 127.0.0.1:5346'
want 'append command --local-server 127.0.0.1:53'
want 'append command --local-domain lan'
want 'append command --cache-db /tmp/cpxy/dns-iot.sqlite'
[ "$(grep -c -- '--local-domain lan' "$LOG")" = 3 ] || fail 'local domains repeated'
[ "$(grep -c -- '--sinkhole 198.18.0.1' "$LOG")" = 2 ] || fail 'block_ads per instance'
want 'route -4 route replace blackhole 198.18.0.0/24'
[ "$(uci get firewall.cpxy_inst_main_fwd1.src)" = trusted ] || fail 'source zone lookup'
[ "$(uci get firewall.cpxy_inst_guest_zone.device)" = cpxy1 ] || fail 'TUN zone'
[ "$(uci get firewall.cpxy_inst_guest_udp.target)" = REJECT ] || fail 'UDP rejection'
[ "$(uci get firewall.cpxy_inst_guest_dns1.src)" = visitors ] || fail 'DNS redirect zone'
[ "$(uci get firewall.cpxy_inst_guest_dns1.dest_port)" = 5345 ] || fail 'DNS redirect port'
[ "$(uci get firewall.cpxy_inst_guest_dns1.target)" = DNAT ] || fail 'DNS redirect target'
[ "$(uci get firewall.cpxy_inst_iot_dns6_1.target)" = REJECT ] || fail 'IPv6 DNS refusal'
# cpxy-tsgw: main and guest only
want 'set command /usr/bin/cpxy-tsgw'
want 'TS_HOSTNAME=cpxy-guest TS_PORT=41642'
want 'DNS_UPSTREAM=223.5.5.5,119.29.29.29 DNS_ALTERNATIVE=127.0.0.1:5336 BLOCK_ADS=1'
want 'append env HTTPS_PROXY=http://127.0.0.1:5001 HTTP_PROXY=http://127.0.0.1:5001'
absent 'TS_HOSTNAME=cpxy-iot'
[ -d /etc/cpxy/tsgw/guest ] || fail 'tsgw state directory'
absent 'TS_AUTHKEY'

# Idempotent reload preserves resource ownership and avoids firewall reloads.
: >"$LOG"
service cpxy-router reload_service
absent 'firewall reload'
absent 'down '
absent 'up '
[ -z "$(uci -q changes firewall)" ] || fail 'idempotent reload staged firewall changes'

# Conflicts between instances
uci set cpxy.guest.index=0
invalid 'index 0 is already used'
uci set cpxy.guest.index=1
uci set cpxy.guest.client_http_listen='[::]:5000'
invalid 'tcp 5000 is already used'
uci delete cpxy.guest.client_http_listen
uci set cpxy.guest.smartdns_listen=127.0.0.1:53
invalid 'must not use port 53'
uci delete cpxy.guest.smartdns_listen
uci add_list cpxy.guest.smartdns_server=tls://dns.google
invalid "smartdns_server 'tls://dns.google' must be an IP address"
uci delete cpxy.guest.smartdns_server
uci add_list cpxy.guest.smartdns_server=8.8.8.8
invalid "smartdns_server '8.8.8.8' must be an IP address, over tcp://"
uci delete cpxy.guest.smartdns_server
uci set cpxy.guest.client=0
invalid 'smartdns_proxy needs'
uci set cpxy.guest.client=1
uci set cpxy.guest.smartdns=0
invalid "'dns_alternative' is required"
uci set cpxy.guest.smartdns=1
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
service cpxy-router reload_service
uci set cpxy.main.router_dns=1
uci add_list cpxy.main.network=extra
uci add_list cpxy.guest.network=extra
: >/tmp/err
if service cpxy-router reload_service 2>/tmp/err; then fail 'network in two instances accepted'; fi
grep -qF "network 'extra' is assigned more than once" /tmp/err || fail 'missing error: network twice'
uci delete cpxy.guest.network
uci add_list cpxy.guest.network=guest
service cpxy-router reload_service

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
# No instance blocks ads: no sinkhole route
uci set cpxy.defaults.block_ads=0
: >"$LOG"
service cpxy-router reload_service
want 'route -4 route del blackhole 198.18.0.0/24'

# Interface triggers for every routed network
procd_add_reload_trigger() { :; }
procd_add_interface_trigger() { echo "trigger $*" >>"$LOG"; }
: >"$LOG"
service cpxy-router service_triggers
want 'trigger interface.* lan /etc/init.d/cpxy-router reload'
want 'trigger interface.* iot /etc/init.d/cpxy-router reload'
absent 'trigger interface.* guest'

service cpxy-router stop_service
[ ! -d /tmp/cpxy/instances ] || fail 'runtime state remains'
if uci show firewall | grep -q cpxy_inst_; then fail 'firewall sections remain after stop'; fi
echo 'multi-instance init and firewall checks passed'
SCRIPT
)"
"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
