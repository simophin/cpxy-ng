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

# Only package-owned sections are removed; existing network forwarding is untouched.
cpxy_firewall_clear() {
	local section
	for section in $(uci -q show firewall | sed -n 's/^firewall\.\([^.=]*\)=.*/\1/p'); do
		case "$section" in cpxy_inst_*|cpxy_zone|cpxy_fwd|cpxy_udp) uci -q delete "firewall.$section" ;; esac
	done
	return 0
}

# cpxy_firewall_add <section> <tun> <table> <source zones...>
cpxy_firewall_add() {
	local section="$1" tun="$2" src base="cpxy_inst_$1" zone="cx$3" n=0
	shift 3
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
	done
}

# Stage the desired firewall in an isolated UCI directory. Identical reloads do not
# create pending UCI edits or commit unrelated user edits.
cpxy_firewall_sync() (
	local plan="$1" work state section tun table zones
	work="$(mktemp -d /tmp/cpxy-firewall.XXXXXX)" || return 1
	trap 'rm -rf "$work"' EXIT
	mkdir "$work/delta"
	uci -q export firewall >"$work/firewall" || return 1
	cp "$work/firewall" "$work/before"
	(
		uci() { command uci -c "$work" -P "$work/delta" "$@"; }
		cpxy_firewall_clear
		if [ -n "$plan" ]; then
			for state in "$plan"/*; do
				[ -f "$state" ] || continue
				section="${state##*/}"
				{ read -r tun; read -r table; read -r _; read -r _; read -r zones; } <"$state"
				cpxy_firewall_add "$section" "$tun" "$table" $zones || exit 1
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
