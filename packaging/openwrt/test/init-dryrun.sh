#!/bin/sh
# Exercise multi-instance supervision and real UCI firewall generation in an OpenWrt rootfs.
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
# Names intentionally differ from network names. One zone contains multiple networks.
uci set firewall.home=zone
uci set firewall.home.name=trusted
uci add_list firewall.home.network=lan
uci add_list firewall.home.network=extra
uci set firewall.visitors=zone
uci set firewall.visitors.name=visitors
uci set firewall.visitors.network=guest
uci commit firewall
uci set cpxy.main.enabled=1
uci set cpxy.main.server='https://:test-key@proxy.example:443'
uci add_list cpxy.main.dns_upstream=192.0.2.20
uci commit cpxy
LOG=/tmp/calls.txt
: >"$LOG"
cat >/etc/init.d/firewall <<'FW'
#!/bin/sh
echo "firewall $*" >>/tmp/calls.txt
FW
chmod +x /etc/init.d/firewall
. /lib/functions.sh
. /etc/init.d/cpxy
procd_open_instance() { echo "instance $1" >>"$LOG"; }
procd_close_instance() { :; }
procd_set_param() { echo "set $*" >>"$LOG"; }
procd_append_param() { echo "append $*" >>"$LOG"; }
procd_kill() { echo "FAIL: procd_kill would destroy the current service update"; exit 1; }
rc_procd() { "$@"; }
network_get_device() { case "$2" in lan|extra) eval "$1=br-lan" ;; guest) eval "$1=br-guest" ;; esac; }
logger() { :; }
mkdir /tmp/tuns
ip() {
	case "$1 $2" in
		'link show') [ -f "/tmp/tuns/$3" ] ;;
		'tuntap show') echo "$4: tun" ;;
		*) command ip "$@" ;;
	esac
}
cpxy_net_up() { touch "/tmp/tuns/$CPXY_TUN"; echo "up $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN $*" >>"$LOG"; }
cpxy_net_down_routing() { rm -f "/tmp/tuns/$CPXY_TUN"; echo "down $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN" >>"$LOG"; }
_cpxy_net_down_rules() { echo "rules_down $CPXY_TUN $CPXY_TABLE $CPXY_PRIO_MAIN" >>"$LOG"; }
cpxy_dnsmasq_down() { :; }
fail() { echo "FAIL: $*"; cat "$LOG"; cat /tmp/err 2>/dev/null; exit 1; }
want() { grep -qF -- "$1" "$LOG" || fail "expected: $1"; }
absent() { if grep -qF -- "$1" "$LOG"; then fail "unexpected: $1"; fi; }
invalid() {
	: >"$LOG"
	if reload_service 2>/tmp/err; then fail "accepted invalid configuration"; fi
	[ ! -s "$LOG" ] || fail "invalid reload changed running service"
	grep -qF "$1" /tmp/err || fail "missing error: $1"
}
invalid "dns_alternative' is required"
uci set cpxy.main.dns_split=0
start_service
want 'instance main_tun'
want 'up cpxy0 100 9100 br-lan'
absent 'instance dns'
[ "$(uci get firewall.cpxy_inst_main_fwd1.src)" = trusted ] || fail 'source zone lookup'
[ "$(uci get firewall.cpxy_inst_main_zone.device)" = cpxy0 ] || fail 'TUN zone'
[ "$(uci get firewall.cpxy_inst_main_udp.target)" = REJECT ] || fail 'UDP rejection'

uci set cpxy.guest=cpxy
uci set cpxy.guest.enabled=1
uci set cpxy.guest.server='https://:other-key@second.example:443'
uci add_list cpxy.guest.lan_interface=guest
uci set cpxy.guest.tun=cpxy1
uci set cpxy.guest.routing_table=101
uci set cpxy.guest.rule_priority=9110
: >"$LOG"
reload_service
want 'instance main_tun'
want 'instance guest_tun'
want 'up cpxy1 101 9110 br-guest'
want 'set command /usr/bin/cpxy-router --tun cpxy1'
want 'set env SERVER=https://:other-key@second.example:443 NO_COLOR=1'
[ "$(uci get firewall.cpxy_inst_guest_fwd1.src)" = visitors ] || fail 'guest zone lookup'
[ "$(uci get firewall.cpxy_inst_guest_fwd1.dest)" = cx101 ] || fail 'guest forwarding'
# Idempotent reload preserves resource ownership and avoids firewall reloads/worker kills.
: >"$LOG"
reload_service
absent 'firewall reload'
absent 'kill '
absent 'down '
absent 'up '
[ -z "$(uci -q changes firewall)" ] || fail 'idempotent reload staged firewall changes'

uci set cpxy.guest.routing_table=100
invalid 'duplicate routing_table'
uci set cpxy.guest.routing_table=101
uci set cpxy.guest.rule_priority=9102
invalid 'overlapping rule priorities'
uci set cpxy.guest.rule_priority=9110
uci set cpxy.guest.tun=cpxy0
invalid 'duplicate tun'
uci set cpxy.guest.tun=cpxy1
uci set cpxy.guest.tun=lo
invalid 'invalid or missing tun'
uci set cpxy.guest.tun=cpxy1
uci delete cpxy.guest.lan_interface
uci add_list cpxy.guest.lan_interface=lan
invalid "network 'lan' is assigned more than once"
uci delete cpxy.guest.lan_interface
uci add_list cpxy.guest.lan_interface=extra
invalid "device 'br-lan' is assigned more than once"
uci delete cpxy.guest.lan_interface
uci add_list cpxy.guest.lan_interface=unknown
invalid 'exactly one enabled firewall zone'
uci delete cpxy.guest.lan_interface
uci add_list cpxy.guest.lan_interface=guest
uci set firewall.duplicate=zone
uci set firewall.duplicate.name=other
uci add_list firewall.duplicate.network=guest
invalid 'exactly one enabled firewall zone'
uci delete firewall.duplicate
uci set cpxy.main.dns_split=1
uci add_list cpxy.main.dns_alternative=tls://192.0.2.30
uci set cpxy.guest.dns_split=1
uci add_list cpxy.guest.dns_upstream=192.0.2.20
uci add_list cpxy.guest.dns_alternative=192.0.2.30
invalid 'only one instance'
uci set cpxy.guest.dns_split=0
: >"$LOG"
reload_service
want 'instance dns'
want 'append command --cache-db /tmp/cpxy/dns-cache.sqlite'
# Disable guest without tearing down main routing or its worker.
uci set cpxy.guest.enabled=0
: >"$LOG"
reload_service
want 'down cpxy1 101 9110'
absent 'down cpxy0'
absent 'kill cpxy main_tun'
absent 'instance guest_tun'
if uci -q get firewall.cpxy_inst_guest_zone; then fail 'disabled guest zone remains'; fi
[ "$(uci get firewall.cpxy_inst_main_fwd1.src)" = trusted ] || fail 'main forwarding removed'
# Re-enable and reassign guest resources; cleanup uses the saved old assignments.
uci set cpxy.guest.enabled=1
reload_service
uci set cpxy.guest.tun=cpxy2
uci set cpxy.guest.routing_table=102
uci set cpxy.guest.rule_priority=9120
: >"$LOG"
reload_service
want 'down cpxy1 101 9110'
want 'up cpxy2 102 9120 br-guest'
absent 'down cpxy0'
uci delete cpxy.guest
: >"$LOG"
reload_service
want 'down cpxy2 102 9120'
absent 'down cpxy0'
# All configured interfaces get triggers, including a network with no device yet.
procd_add_reload_trigger() { :; }
procd_add_interface_trigger() { echo "trigger $*" >>"$LOG"; }
uci set cpxy.guest=cpxy
uci set cpxy.guest.enabled=1
uci add_list cpxy.guest.lan_interface=guest
: >"$LOG"
service_triggers
want 'trigger interface.* guest /etc/init.d/cpxy reload'
want 'trigger interface.* lan /etc/init.d/cpxy reload'
stop_service
[ ! -d /tmp/cpxy ] || fail 'runtime state remains'
if uci -q get firewall.cpxy_inst_main_zone; then fail 'zone remains after stop'; fi
echo 'multi-instance init and firewall checks passed'
SCRIPT
)"
"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
