#!/bin/sh
# shellcheck disable=SC3043,SC2154
# BusyBox ash supports local; config_get sets named variables.
# Single opt-in VPN policy. Fixed owned namespace; no dependency on old cpxy files.
set -e
. /lib/functions.sh
. /lib/functions/network.sh
R=/var/run/cpxy-rtp
P=/etc/cpxy-rtp
TUN=rtpr0
TABLE=20000
STAGE=30000
PRIO=12000
DNS=5353
fail() { echo "cpxy-rtp: $*" >&2; exit 1; }
safe() { case "$1" in ''|*[!A-Za-z0-9_.:-]*) return 1;; esac; }
setting() {
    local value
    value="$(uci -c "$CONF" -t "$R/deltas" -q get "cpxy_rtp.main.$1")" || value="${2:-}"
    printf '%s\n' "$value"
}
load() {
    CONF="$1"
    enabled="$(setting enabled 0)"
    entry="$(setting entry)"
    password="$(setting password)"
    networks="$(setting network)"
    upstream="$(setting dns_upstream 223.5.5.5)"
    alternative="$(setting dns_alternative '8.8.8.8 1.1.1.1')"
    local_server="$(setting dns_local_server 127.0.0.1:53)"
    local_domains="$(setting dns_local_domain lan)"
    search_domains="$(setting dns_search_domain)"
    proxies="$(setting proxy_route)"
    ipv6_direct="$(setting ipv6_direct 0)"
}
flush_rules() {
	local fam n
	for fam in -4 -6; do
		for n in 0 1 2 3 4; do
			while ip "$fam" rule del priority "$((PRIO+n))" 2>/dev/null; do :; done
		done
	done
}
guard() {
	local dev fam
	for dev in $devices; do
		for fam in -4 -6; do
			if [ "$fam" = -6 ] && [ -s "$P/routes6" ]; then
				# The nft guard is already installed before releasing this terminal rule.
				# Normal IPv6 routing must reach the forward hook to send TCP resets.
				while ip -6 rule del priority "$((PRIO+4))" iif "$dev" prohibit 2>/dev/null; do :; done
				continue
			fi
			ip "$fam" rule show priority "$((PRIO+4))" | grep -qF "iif $dev " ||
				ip "$fam" rule add priority "$((PRIO+4))" iif "$dev" prohibit
			ip "$fam" rule show priority "$PRIO" | grep -qF "iif $dev " ||
				ip "$fam" rule add priority "$PRIO" iif "$dev" lookup main suppress_prefixlength 0
		done
		for spec in 'udp 443' 'tcp 853'; do
			set -- $spec
			ip -4 rule show priority "$((PRIO+1))" | grep -qF "iif $dev ipproto $1 dport $2 " ||
				ip -4 rule add priority "$((PRIO+1))" iif "$dev" ipproto "$1" dport "$2" prohibit
		done
	done
}
zone_for() {
	local section="$1" name members
	config_get name "$section" name
	config_get members "$section" network
	case " $members " in *" $wanted "*) zones="$zones $name"; zone_members="$members";; esac
}
old_networks() {
	local section="$1" on router members inherited olddev
	config_get on "$section" enabled 1
	config_get inherited defaults router 1
	config_get router "$section" router "$inherited"
	config_get members "$section" network
	[ "$on" = 1 ] && [ "$router" = 1 ] || return 0
	for member in $members; do
		case " $networks " in *" $member "*) fail "network $member belongs to enabled cpxy-router; disable it there first";; esac
		network_get_device olddev "$member" || olddev="$(uci -q get "network.$member.device" || :)"
		case " $devices " in *" $olddev "*) fail "device $olddev belongs to enabled cpxy-router";; esac
	done
}
engine() {
	local check_mode="${1:-}"
	set -- /usr/bin/cpxy-rtp-router --entry "$entry" --interface "$TUN" --state-dir "$P/state/main" --dns-listen "0.0.0.0:$DNS" --dns-local-server "$local_server"
	for value in $upstream; do set -- "$@" --dns-upstream "$value"; done
	for value in $alternative; do set -- "$@" --dns-alternative "$value"; done
	for value in $local_domains; do set -- "$@" --dns-local-domain "$value"; done
	for value in $search_domains; do set -- "$@" --dns-search-domain "$value"; done
	[ "$ipv6_direct" != 1 ] || set -- "$@" --ipv6-direct
	[ "${check_mode:-}" != check ] || set -- "$@" --check
	CPXY_RTP_PASSWORD="$password" exec "$@"
}
validate() {
	case "$ipv6_direct" in 0|1) :;; *) fail 'ipv6_direct must be 0 or 1';; esac
	[ -n "$entry" ] && [ "${#password}" -ge 12 ] || fail 'set entry and a password of at least 12 characters'
	(engine check) || fail 'invalid transport or DNS configuration'
	[ -n "$networks" ] || fail 'select at least one network'
	for option in flow_offloading flow_offloading_hw; do
		[ "$(uci -q get "firewall.@defaults[0].$option" || :)" != 1 ] || fail "disable $option first"
	done
	devices='' zones_all=''
	for wanted in $networks; do
		safe "$wanted" || fail 'invalid network'
		network_get_device dev "$wanted" || dev="$(uci -q get "network.$wanted.device" || :)"
		safe "$dev" && [ "$dev" != "$TUN" ] || fail "network $wanted needs an explicit device"
		case " $devices " in *" $dev "*) fail 'duplicate network device';; esac
		devices="$devices $dev"
		zones='' zone_members=''
		config_load firewall
		config_foreach zone_for zone
		set -- $zones
		[ "$#" = 1 ] || fail "network $wanted must belong to exactly one firewall zone"
		safe "$1" || fail 'invalid zone'
		case " $zones_all " in *" $1 "*) :;; *) zones_all="$zones_all $1";; esac
		for member in $zone_members; do
			case " $networks " in *" $member "*) :;; *) fail 'select every network in each selected zone';; esac
		done
	done
	if [ -f /etc/config/cpxy ]; then
		config_load cpxy
		config_foreach old_networks instance
	fi
	# Export validates forced IPv4 CIDRs before any mutation.
	set -- --export-direct-routes
	for prefix in $proxies; do set -- "$@" --proxy-route "$prefix"; done
	/usr/bin/cpxy-rtp-router "$@" >"$R/routes.new" || fail 'route export failed'
	if [ "$ipv6_direct" = 1 ]; then
		/usr/bin/cpxy-rtp-router --export-direct-routes6 >"$R/routes6.new" || fail 'IPv6 route export failed'
		[ -s "$R/routes6.new" ] || fail 'IPv6 regional data is empty'
	fi
	if [ -f "$P/devices" ]; then
		[ "$(cat "$P/devices")" = "$devices" ] || fail 'cleanup before changing selected devices'
	else
		! nft list table inet cpxy_rtp_guard >/dev/null 2>&1 || fail 'guard table already occupied'
		[ -z "$(uci show firewall | sed -n 's/^firewall\.cpxy_rtp_[^=]*=.*/occupied/p')" ] || fail 'firewall sections already occupied'
		! ip link show "$TUN" >/dev/null 2>&1 || fail 'rtpr0 already exists'
		for t in "$TABLE" "$STAGE"; do
			[ -z "$(ip -4 route show table "$t" 2>/dev/null || :)" ] || fail "table $t already occupied"
		done
		for n in 0 1 2 3 4; do
			[ -z "$(ip -4 rule show priority "$((PRIO+n))")$(ip -6 rule show priority "$((PRIO+n))")" ] || fail 'routing priorities occupied'
		done
	fi
}
firewall() {
	uci -q batch <<EOT
set firewall.cpxy_rtp_guard=include
set firewall.cpxy_rtp_guard.type='script'
set firewall.cpxy_rtp_guard.path='/usr/libexec/cpxy-rtp/guard.sh'
set firewall.cpxy_rtp_guard.fw4_compatible='1'
set firewall.cpxy_rtp_zone=zone
set firewall.cpxy_rtp_zone.name='rtpvpn'
set firewall.cpxy_rtp_zone.device='$TUN'
set firewall.cpxy_rtp_zone.input='REJECT'
set firewall.cpxy_rtp_zone.output='ACCEPT'
set firewall.cpxy_rtp_zone.forward='REJECT'
set firewall.cpxy_rtp_zone.masq='1'
set firewall.cpxy_rtp_zone.mtu_fix='1'
EOT
	local z i=0
	for z in $zones_all; do
		i=$((i+1))
		uci -q batch <<EOT
set firewall.cpxy_rtp_forward$i=forwarding
set firewall.cpxy_rtp_forward$i.src='$z'
set firewall.cpxy_rtp_forward$i.dest='rtpvpn'
set firewall.cpxy_rtp_dns$i=redirect
set firewall.cpxy_rtp_dns$i.name='RTP DNS $i'
set firewall.cpxy_rtp_dns$i.src='$z'
set firewall.cpxy_rtp_dns$i.src_dport='53'
set firewall.cpxy_rtp_dns$i.dest_port='$DNS'
set firewall.cpxy_rtp_dns$i.proto='tcp udp'
set firewall.cpxy_rtp_dns$i.target='DNAT'
set firewall.cpxy_rtp_dns$i.family='ipv4'
set firewall.cpxy_rtp_input$i=rule
set firewall.cpxy_rtp_input$i.name='RTP DNS input $i'
set firewall.cpxy_rtp_input$i.src='$z'
set firewall.cpxy_rtp_input$i.dest_port='$DNS'
set firewall.cpxy_rtp_input$i.proto='tcp udp'
set firewall.cpxy_rtp_input$i.target='ACCEPT'
set firewall.cpxy_rtp_input$i.family='ipv4'
EOT
	done
	uci commit firewall
	/etc/init.d/firewall reload
}
uplink() {
	ip -4 route show default | awk '
	/^default / {n++; for(i=1;i<=NF;i++){if($i=="via")g=$(i+1);if($i=="dev")d=$(i+1)}}
	END {if(n>1)exit 1;if(!n)print "- -";else if(d!="")print (g==""?"-":g),d;else exit 1}'
}
reconcile() {
	local wan="$1" present="$2" active stage prefix gateway device
	set -- $wan; gateway="$1"; device="$2"
	active="$(ip -4 rule show priority "$((PRIO+2))" | awk '/lookup/{print $NF;exit}')"
	stage="$TABLE"; [ "$active" != "$TABLE" ] || stage="$STAGE"
	ip -4 route flush table "$stage" 2>/dev/null || :
	ip -4 route add unreachable default table "$stage" metric 1000
	[ "$present" = no ] || ip -4 route add default dev "$TUN" table "$stage" metric 10
	awk -v t="$stage" -v g="$gateway" -v d="$device" '
	{ print "route add unreachable",$0,"metric 1000 table",t
	if(d!="-"){if(g=="-")print "route add",$0,"dev",d,"metric 10 table",t
	else print "route add",$0,"via",g,"dev",d,"onlink metric 10 table",t}}' "$R/routes" >"$R/batch"
	ip -4 -batch "$R/batch" </dev/null
	while ip -4 rule del priority "$((PRIO+2))" 2>/dev/null; do :; done
	for dev in $devices; do ip -4 rule add priority "$((PRIO+2))" iif "$dev" lookup "$stage"; done
	[ -z "$active" ] || ip -4 route flush table "$active" 2>/dev/null || :
}
case "${1:-}" in
prepare|boot)
	umask 077; mkdir -p "$R" "$P" "$R/next" "$R/config" "$R/deltas"
	rm -f "$R/enabled"
	# Restore persisted guards even when configuration is disabled at boot.
	if [ -f "$P/devices" ]; then devices="$(cat "$P/devices")"; /usr/libexec/cpxy-rtp/guard.sh; guard; fi
	uci export cpxy_rtp | sed '/^package /d' >"$R/next/cpxy_rtp"
	load "$R/next"; [ "$enabled" = 1 ] || exit 0
	validate
	mv "$R/next/cpxy_rtp" "$R/config/cpxy_rtp"
	printf '%s\n' "$devices" >"$P/devices"
	cp "$R/routes.new" "$P/routes.new"
	if ! cmp -s "$P/routes.new" "$P/routes"; then mv "$P/routes.new" "$P/routes"; else rm "$P/routes.new"; fi
	if [ "$ipv6_direct" = 1 ]; then cp "$R/routes6.new" "$P/routes6.new"; mv "$P/routes6.new" "$P/routes6"; else rm -f "$P/routes6"; fi
	/usr/libexec/cpxy-rtp/guard.sh
	guard
	mv "$R/routes.new" "$R/routes"
	if [ "$1" = prepare ]; then
		firewall
		touch "$R/enabled"
	fi
	;;
run)
	load "$R/config"; devices="$(cat "$P/devices")"
	engine & child=$!
	unset password
	trap 'kill "$child" 2>/dev/null || :; wait "$child" || :; exit 0' TERM INT EXIT
	last=
	while kill -0 "$child" 2>/dev/null; do
		wan="$(uplink)" || wan='- -'
		present=no; ip link show dev "$TUN" 2>/dev/null | grep -q 'UP' && present=yes
		guard
		for dev in $devices; do
			ip -4 rule show priority "$((PRIO+2))" | grep -qF "iif $dev " || last=
		done
		fingerprint="$wan $present"
		if [ "$last" != "$fingerprint" ]; then
			reconcile "$wan" "$present"
			[ -z "$last" ] || kill -USR1 "$child"
			last="$fingerprint"
		fi
		sleep 1
	done
	wait "$child"
	;;
cleanup)
	[ -f "$P/devices" ] || exit 0
	flush_rules
	nft delete table inet cpxy_rtp_guard 2>/dev/null || :
	ip -4 route flush table "$TABLE" 2>/dev/null || :
	ip -4 route flush table "$STAGE" 2>/dev/null || :
	for section in $(uci show firewall | sed -n 's/^firewall\.\(cpxy_rtp_[A-Za-z0-9_]*\)=.*/\1/p'); do uci delete "firewall.$section"; done
	uci commit firewall
	/etc/init.d/firewall reload
	rm -f "$P/devices" "$P/routes" "$P/routes6" "$R/enabled"
	;;
*) fail 'expected prepare, run or cleanup';;
esac
