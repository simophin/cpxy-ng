#!/bin/sh
# The dnsmasq drop-in that hands LAN DNS to dns_split, and the sinkhole route for blocked names.
# Sourced by /etc/init.d/cpxy; the drop-in lives in /tmp, so a reboot removes it.

CPXY_DNSMASQ_DROPIN=cpxy.conf

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

# Names on the ad blocklist resolve into this range (dns_split --sinkhole). The kernel drops what
# LAN devices send to a blackhole route without an ICMP error, so blocked connections hang instead
# of failing. Not a private range: dnsmasq's rebind protection would discard those answers.
# Kept in step with mobile-engine/src/config.rs.
# shellcheck disable=SC2034 # read by /etc/init.d/cpxy, which sources this
CPXY_SINKHOLE_ADDR=198.18.0.1
CPXY_SINKHOLE_NET=198.18.0.0/24

cpxy_sinkhole_up() {
	ip -4 route replace blackhole "$CPXY_SINKHOLE_NET"
}

cpxy_sinkhole_down() {
	ip -4 route del blackhole "$CPXY_SINKHOLE_NET" 2>/dev/null
	return 0
}
