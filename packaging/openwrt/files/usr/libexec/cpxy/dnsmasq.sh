#!/bin/sh
# The dnsmasq drop-in that hands LAN DNS to dns_split. Sourced by /etc/init.d/cpxy; the drop-in
# lives in /tmp, so a reboot removes it.

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
