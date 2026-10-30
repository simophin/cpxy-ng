#!/bin/sh
# Network membership, rather than zone names, selects the source firewall zone.
cpxy_firewall_zone() (
	local wanted="$1" matches=0 found=""
	_zone() {
		local name active networks network member=0
		config_get_bool active "$1" enabled 1
		[ "$active" = 1 ] || return 0
		# fw3 accepts both option network 'guest' and list network 'guest'.
		config_get networks "$1" network
		for network in $networks; do
			[ "$network" = "$wanted" ] && member=1
		done
		[ "$member" = 1 ] || return 0
		config_get name "$1" name
		[ -n "$name" ] || return 0
		found="$name"
		matches=$((matches + 1))
	}
	config_load firewall
	config_foreach _zone zone
	[ "$matches" = 1 ] || return 1
	printf '%s\n' "$found"
)

# The networks of the enabled zone named $1
cpxy_firewall_zone_networks() (
	local wanted="$1"
	_zone() {
		local name active networks
		config_get_bool active "$1" enabled 1
		[ "$active" = 1 ] || return 0
		config_get name "$1" name
		[ "$name" = "$wanted" ] || return 0
		config_get networks "$1" network
		echo $networks
	}
	config_load firewall
	config_foreach _zone zone
)

# Only package-owned sections are removed; existing network forwarding is untouched.
cpxy_firewall_clear() {
	local section
	for section in $(uci -q show firewall | sed -n 's/^firewall\.\([^.=]*\)=.*/\1/p'); do
		case "$section" in cpxy_inst_*) uci -q delete "firewall.$section" ;; esac
	done
	return 0
}

# cpxy_firewall_add <section> <tun> <table> <dns port, or -> <source zones...>
# With a DNS port, IPv4 and IPv6 DNS goes to the instance split resolver.
cpxy_firewall_add() {
	local section="$1" tun="$2" dns="$4" src base="cpxy_inst_$1" zone="cx$3" n=0
	shift 4
	uci -q batch <<UCI
set firewall.${base}_zone=zone
set firewall.${base}_zone.name='$zone'
add_list firewall.${base}_zone.device='$tun'
set firewall.${base}_zone.input='REJECT'
set firewall.${base}_zone.output='ACCEPT'
set firewall.${base}_zone.forward='REJECT'
set firewall.${base}_udp=rule
set firewall.${base}_udp.name='cpxy $section UDP refusal'
set firewall.${base}_udp.src='*'
set firewall.${base}_udp.dest='$zone'
set firewall.${base}_udp.proto='udp'
set firewall.${base}_udp.target='REJECT'
UCI
	for src in "$@"; do
		n=$((n + 1))
		uci -q batch <<UCI
set firewall.${base}_fwd$n=forwarding
set firewall.${base}_fwd$n.src='$src'
set firewall.${base}_fwd$n.dest='$zone'
UCI
		[ "$dns" != - ] || continue
		uci -q batch <<UCI
set firewall.${base}_dns$n=redirect
set firewall.${base}_dns$n.name='cpxy $section DNS'
set firewall.${base}_dns$n.src='$src'
set firewall.${base}_dns$n.family='ipv4'
set firewall.${base}_dns$n.proto='tcp udp'
set firewall.${base}_dns$n.src_dport='53'
set firewall.${base}_dns$n.dest_port='$dns'
set firewall.${base}_dns$n.target='DNAT'
set firewall.${base}_dns6_$n=redirect
set firewall.${base}_dns6_$n.name='cpxy $section IPv6 DNS'
set firewall.${base}_dns6_$n.src='$src'
set firewall.${base}_dns6_$n.family='ipv6'
set firewall.${base}_dns6_$n.proto='tcp udp'
set firewall.${base}_dns6_$n.src_dport='53'
set firewall.${base}_dns6_$n.dest_port='$dns'
set firewall.${base}_dns6_$n.target='DNAT'
UCI
	done
}

# The position of the first rule section in the firewall configuration
_cpxy_firewall_first_rule() {
	uci -q show firewall | grep -E '^firewall\.[^.]+=' | awk -F= '$2 == "rule" { print NR - 1; exit }'
}

# Stage the desired firewall in an isolated UCI directory. Identical reloads do not
# create pending UCI edits or commit unrelated user edits.
cpxy_firewall_sync() (
	local plan="$1" work state section tun table zones dns
	work="$(mktemp -d /tmp/cpxy-firewall.XXXXXX)" || return 1
	trap 'rm -rf "$work"' EXIT
	mkdir "$work/delta"
	uci -q export firewall >"$work/firewall" || return 1
	cp "$work/firewall" "$work/before"
	(
		uci() { command uci -c "$work" -P "$work/delta" "$@"; }
		cpxy_firewall_clear
		# Keep this include while next-boot assignments exist, including an explicit
		# service stop. Its runtime stopped marker allows ordinary forwarding then.
		uci -q delete firewall.cpxy_boot_guard
		for state in /etc/cpxy/router-guards/*; do
			[ -f "$state" ] || continue
			uci -q batch <<UCI
set firewall.cpxy_boot_guard=include
set firewall.cpxy_boot_guard.type='script'
set firewall.cpxy_boot_guard.path='/usr/libexec/cpxy/boot-firewall.sh'
set firewall.cpxy_boot_guard.fw4_compatible='1'
set firewall.cpxy_boot_guard.reload='1'
UCI
			break
		done
		if [ -n "$plan" ]; then
			for state in "$plan"/*; do
				[ -f "$state" ] || continue
				section="${state##*/}"
				{ read -r tun; read -r table; read -r _; read -r _; read -r zones; read -r dns; } <"$state"
				cpxy_firewall_add "$section" "$tun" "$table" "$dns" $zones || exit 1
			done
		fi
		uci -q export firewall >"$work/after"
	) || return 1
	if ! cmp -s "$work/before" "$work/after"; then
		uci import firewall <"$work/after" || return 1
		uci commit firewall || return 1
		/etc/init.d/firewall reload >/dev/null 2>&1 || return 1
	fi
)
