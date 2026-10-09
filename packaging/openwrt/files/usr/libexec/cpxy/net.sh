#!/bin/sh
# Runtime network state for cpxy: the TUN device, policy routing and the dnsmasq drop-in.
# Sourced by /etc/init.d/cpxy. Everything here lives in the kernel or /tmp, so stopping
# the service (or rebooting) removes it; nothing is written to /etc.
#
# Packet path for a proxied LAN device:
#   LAN TCP -> ip rule (prio 9103: table 100) -> default dev cpxy0 -> tun2proxy -> SOCKS5 -> client_cn
#
# cpxy has no UDP path. UDP 443 (QUIC, HTTP/3) is sent to cpxy0 too (prio 9101), where the firewall
# refuses it, so browsers fall back to TCP through the proxy instead of revealing the WAN address.
# All other IPv4 UDP (WebRTC, games, VoIP, DNS to outside resolvers) goes out the WAN (prio 9102).
#
# cpxy0 is created here, not by tun2proxy: without --setup tun2proxy neither addresses nor brings
# up its device, and --setup would reroute the router's own traffic. tun2proxy attaches by name.
# If tun2proxy dies the device stays and drops what it is sent; if the device goes, table 100's
# `unreachable default` refuses LAN traffic. Either way nothing leaks out the WAN.
# Routes for the LAN itself stay in main through the `suppress_prefixlength 0` rules (prio 9100).

CPXY_TUN=cpxy0
CPXY_TABLE=100
CPXY_PRIO_MAIN=9100
CPXY_PRIO_QUIC=9101
CPXY_PRIO_UDP=9102
CPXY_PRIO_TUN=9103
CPXY_DNSMASQ_DROPIN=cpxy.conf

# Delete every rule of ours at a priority, however many there are (one per device and family).
_cpxy_flush_rules() {
	local fam="$1" prio="$2"
	while ip $fam rule del priority "$prio" 2>/dev/null; do :; done
}

# cpxy_net_up <lan device>...
# Also run on every reload (a config change, a LAN interface coming up) while tun2proxy keeps
# running, so an existing cpxy0 is kept: a new device would leave tun2proxy attached to the old one.
cpxy_net_up() {
	local dev fam

	# Idempotent: rules and routes start from a clean slate
	_cpxy_net_down_rules

	ip link show "$CPXY_TUN" >/dev/null 2>&1 || ip tuntap add dev "$CPXY_TUN" mode tun || return 1
	ip link set "$CPXY_TUN" up || return 1

	for fam in -4 -6; do
		ip $fam route replace unreachable default table "$CPXY_TABLE" metric 1000 || return 1
		for dev in "$CPXY_TUN" "$@"; do
			ip $fam rule add priority "$CPXY_PRIO_MAIN" iif "$dev" lookup main suppress_prefixlength 0 || return 1
			ip $fam rule add priority "$CPXY_PRIO_TUN" iif "$dev" lookup "$CPXY_TABLE" || return 1
		done
	done
	# IPv4 only: IPv6 from the LAN stays refused so clients fall back to IPv4
	ip -4 route replace default dev "$CPXY_TUN" table "$CPXY_TABLE" metric 10 || return 1
	for dev in "$@"; do
		ip -4 rule add priority "$CPXY_PRIO_QUIC" iif "$dev" ipproto udp dport 443 lookup "$CPXY_TABLE" || return 1
		ip -4 rule add priority "$CPXY_PRIO_UDP" iif "$dev" ipproto udp lookup main || return 1
	done
}

_cpxy_net_down_rules() {
	local fam
	for fam in -4 -6; do
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_MAIN"
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_QUIC"
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_UDP"
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_TUN"
		ip $fam route flush table "$CPXY_TABLE" 2>/dev/null
	done
}

cpxy_net_down_routing() {
	_cpxy_net_down_rules
	ip link del "$CPXY_TUN" 2>/dev/null
	return 0
}

# Where dnsmasq reads extra config files from (see the dnsmasq init script)
_cpxy_dnsmasq_confdir() {
	local dir name
	dir="$(uci -q get 'dhcp.@dnsmasq[0].confdir')"
	if [ -z "$dir" ]; then
		name="$(uci -q show 'dhcp.@dnsmasq[0]' | sed -n '1s/^dhcp\.\([^.=]*\)=.*/\1/p')"
		dir="/tmp/dnsmasq${name:+.$name}.d"
	fi
	echo "$dir"
}

# cpxy_dnsmasq_up <dns_split listen address as host:port>
# Public queries go to dns_split; DHCP leases and local names still resolve in dnsmasq.
cpxy_dnsmasq_up() {
	local listen="$1" dir host port
	host="${listen%:*}"
	port="${listen##*:}"
	dir="$(_cpxy_dnsmasq_confdir)"
	mkdir -p "$dir" || return 1
	cat >"$dir/$CPXY_DNSMASQ_DROPIN" <<EOF
# Managed by /etc/init.d/cpxy; removed when the service stops.
no-resolv
server=$host#$port
EOF
	/etc/init.d/dnsmasq restart >/dev/null 2>&1
}

cpxy_dnsmasq_down() {
	local dir
	dir="$(_cpxy_dnsmasq_confdir)"
	[ -e "$dir/$CPXY_DNSMASQ_DROPIN" ] || return 0
	rm -f "$dir/$CPXY_DNSMASQ_DROPIN"
	/etc/init.d/dnsmasq restart >/dev/null 2>&1
}
