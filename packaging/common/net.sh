#!/bin/sh
# Runtime network state for cpxy: the TUN device and policy routing. Shared by the OpenWrt
# package (sourced by /etc/init.d/cpxy) and the Debian package (sourced by gateway.sh).
# Everything here lives in the kernel, so stopping the service (or rebooting) removes it.
#
# Packet path for a proxied LAN device:
#   LAN TCP -> ip rule (prio 9103: table 100) -> default dev cpxy0 -> cpxy-router -> shared TCP outbound
#
# cpxy has no UDP path. UDP 443 (QUIC, HTTP/3) is sent to cpxy0 too (prio 9101), where the firewall
# refuses it, so browsers fall back to TCP through the proxy instead of revealing the WAN address.
# All other IPv4 UDP (WebRTC, games, VoIP, DNS to outside resolvers) goes out the WAN (prio 9102).
#
# cpxy0 is created persistently here; the worker attaches by name and leaves routing alone.
# Router-originated outbound sockets therefore use the ordinary WAN routing table.
# If cpxy-router dies the device stays and drops what it is sent; if the device goes, table 100's
# `unreachable default` refuses LAN traffic. Either way nothing leaks out the WAN.
# Routes for the LAN itself stay in main through the `suppress_prefixlength 0` rules (prio 9100).

# Defaults preserve the original main instance and standalone routing lab.
cpxy_net_select() {
	CPXY_TUN="$1"
	CPXY_TABLE="$2"
	CPXY_PRIO_MAIN="$3"
	CPXY_PRIO_QUIC=$(($3 + 1))
	CPXY_PRIO_UDP=$(($3 + 2))
	CPXY_PRIO_TUN=$(($3 + 3))
}
cpxy_net_select cpxy0 100 9100

# Delete every rule of ours at a priority, however many there are (one per device and family).
_cpxy_flush_rules() {
	local fam="$1" prio="$2"
	while ip $fam rule del priority "$prio" 2>/dev/null; do :; done
}

# cpxy_net_up <lan device>...
# Also run on every reload (a config change, a LAN interface coming up) while cpxy-router keeps
# running, so an existing cpxy0 is kept: a new device would leave cpxy-router attached to the old one.
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
