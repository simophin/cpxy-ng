#!/bin/sh
# The kernel side of routed instances on fw3 firmware (OpenWrt 21.02 and older, GL.iNet stock
# 3.x/4.x), which has iptables but no fw4 and no nftables NAT. The same policy as nft.sh:
#
#   IPv4 TCP to a direct address (CN, private, forced routes excluded, main-table routes, and
#   routes through devices other than the uplinks)
#       -> forwarded by the kernel, through the firewall's own LAN-to-WAN forwarding
#   other IPv4 TCP (not to the router itself, not DNS)
#       -> redirected to the instance's cpxy-client transparent listener
#   IPv4 UDP/443 forwarded -> rejected, so QUIC falls back to TCP
#   IPv6 forwarded -> rejected; routed clients are IPv4-only
#   DNS (UDP/TCP 53, both families) -> redirected to the instance's `cpxy dns`
#
# Per instance, an ipset `cpxy_<instance>` (hash:net) holds the direct prefixes. The rules live in
# chains of their own (cpxy_prerouting in nat, cpxy_forward and cpxy_input in filter, for IPv4
# and IPv6), each jumped to first from its built-in chain. fw3 cannot DNAT IPv6, so both DNS
# redirects are here rather than firewall redirect sections. The rendered rules are kept in
# $CPXY_RUN/ipt4 and ipt6 so the firewall include (boot-firewall.sh) restores them after every
# firewall reload.
#
# nft.sh's exemption of routes through non-uplink devices (fib daddr oif) has no iptables match;
# those routes' prefixes are added to the direct set instead (cpxy_ipt_exempt_routes).

CPXY_IPT_CHAINS='nat:PREROUTING:cpxy_prerouting filter:FORWARD:cpxy_forward filter:INPUT:cpxy_input'

_cpxy_fw3_firmware() {
	command -v fw3 >/dev/null
}

# Whether the firmware can route networks the fw3 way.
_cpxy_have_fw3() {
	_cpxy_fw3_firmware && [ -z "$(cpxy_ipt_missing)" ]
}

# What fw3 firmware lacks to route networks, or nothing.
cpxy_ipt_missing() {
	local tool missing=""
	for tool in ipset iptables-restore ip6tables-restore; do
		command -v "$tool" >/dev/null || missing="$missing $tool"
	done
	echo $missing
}

# The ipset holding instance $1's direct prefixes. ipset names are at most 31 characters, and the
# set is loaded under a temporary name ending in _n first.
cpxy_ipt_set() {
	echo "cpxy_$1"
}

cpxy_ipt_section_ok() {
	[ "${#1}" -le 24 ]
}

# cpxy_ipt_exempt_routes <uplink devices...>: the IPv4 prefixes, in any routing table, of routes
# other than the default through a device that is not an uplink (a tunnel such as tailscale0,
# another LAN). Like nft.sh's exempt set, such routes win over the proxy.
cpxy_ipt_exempt_routes() {
	ip -4 route show table all 2>/dev/null | awk -v up=" lo $* " '
		$1 !~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(\/[0-9]+)?$/ { next }
		{
			dev = ""
			for (i = 2; i < NF; i++) if ($i == "dev") dev = $(i + 1)
			if (dev != "" && index(up, " " dev " ") == 0) print $1
		}'
}

# cpxy_ipt_render <plan dir> <output dir> [exempt prefixes...]: writes the ipset restore input
# (ipset, with sets named <set>_n), the IPv4 and IPv6 iptables-restore --noflush inputs (ipt4,
# ipt6), and the instances' set names (sets).
cpxy_ipt_render() (
	local plan="$1" out="$2" state section set dev
	shift 2
	: >"$out/ipset" && : >"$out/sets" || return 1
	for state in "$plan"/*; do
		[ -f "$state" ] || continue
		section="${state##*/}"
		cpxy_nft_read "$state" || return 1
		cpxy_ipt_section_ok "$section" || return 1
		set="$(cpxy_ipt_set "$section")"
		echo "$set" >>"$out/sets"
		printf 'create %s_n hash:net family inet hashsize 1024 maxelem 1048576\nflush %s_n\n' "$set" "$set" >>"$out/ipset"
		{ cat "$plan/routes/$section"; printf '%s\n' "$@"; } | awk -v set="${set}_n" '
			$0 == "" { next }
			$0 !~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(\/[0-9]+)?$/ { bad = 1; exit 1 }
			# hash:net takes /1 to /32; a /0 route would make everything direct
			$0 ~ /\/0$/ { next }
			{ print "add " set " " $0 }
		' >>"$out/ipset" || return 1
	done
	for fam in 4 6; do
		{
			printf '*nat\n:cpxy_prerouting - [0:0]\n-F cpxy_prerouting\n'
			for state in "$plan"/*; do
				[ -f "$state" ] || continue
				section="${state##*/}"
				cpxy_nft_read "$state"
				set="$(cpxy_ipt_set "$section")"
				for dev in $LAN_DEVS; do
					_cpxy_nft_dev_ok "$dev" || return 1
					if [ "$DNS_PORT" != - ]; then
						for proto in udp tcp; do
							printf -- '-A cpxy_prerouting -i %s -p %s --dport 53 -m comment --comment "cpxy dns %s" -j REDIRECT --to-ports %s\n' \
								"$dev" "$proto" "$dev" "$DNS_PORT"
						done
					fi
					[ "$fam" = 4 ] || continue
					printf -- '-A cpxy_prerouting -i %s -p tcp -m addrtype ! --dst-type LOCAL -m set ! --match-set %s dst -m comment --comment "cpxy redirect %s" -j REDIRECT --to-ports %s\n' \
						"$dev" "$set" "$dev" "$PORT"
				done
			done
			printf 'COMMIT\n*filter\n:cpxy_forward - [0:0]\n-F cpxy_forward\n:cpxy_input - [0:0]\n-F cpxy_input\n'
			for state in "$plan"/*; do
				[ -f "$state" ] || continue
				section="${state##*/}"
				cpxy_nft_read "$state"
				set="$(cpxy_ipt_set "$section")"
				for dev in $LAN_DEVS; do
					# The redirected ports, whatever the zone's input policy
					for port in $([ "$fam" = 6 ] || echo "$PORT") $([ "$DNS_PORT" = - ] || echo "$DNS_PORT"); do
						printf -- '-A cpxy_input -i %s -p tcp --dport %s -m conntrack --ctstate DNAT -j ACCEPT\n' "$dev" "$port"
					done
					[ "$DNS_PORT" = - ] ||
						printf -- '-A cpxy_input -i %s -p udp --dport %s -m conntrack --ctstate DNAT -j ACCEPT\n' "$dev" "$DNS_PORT"
					if [ "$fam" = 6 ]; then
						printf -- '-A cpxy_forward -i %s -m comment --comment "cpxy ipv6 %s" -j REJECT\n' "$dev" "$dev"
						continue
					fi
					printf -- '-A cpxy_forward -i %s -p udp --dport 443 -m comment --comment "cpxy quic %s" -j REJECT\n' "$dev" "$dev"
					printf -- '-A cpxy_forward -i %s -p tcp -m set ! --match-set %s dst -m comment --comment "cpxy guard %s" -j REJECT --reject-with tcp-reset\n' \
						"$dev" "$set" "$dev"
				done
			done
			printf 'COMMIT\n'
		} >"$out/ipt$fam" || return 1
	done
)

# _cpxy_ipt <4|6> <iptables args...>
_cpxy_ipt() {
	local fam="$1"
	shift
	if [ "$fam" = 6 ]; then ip6tables -w "$@"; else iptables -w "$@"; fi
}

# Each chain first in its built-in chain, once
_cpxy_ipt_jumps() {
	local fam entry table builtin chain
	for fam in 4 6; do
		for entry in $CPXY_IPT_CHAINS; do
			IFS=: read -r table builtin chain <<EOF
$entry
EOF
			_cpxy_ipt "$fam" -t "$table" -C "$builtin" -j "$chain" 2>/dev/null ||
				_cpxy_ipt "$fam" -t "$table" -I "$builtin" 1 -j "$chain" || return 1
		done
	done
}

# cpxy_ipt_load <dir>: loads rendered ipt4/ipt6 and adds the jumps.
cpxy_ipt_load() {
	iptables-restore -w --noflush <"$1/ipt4" && ip6tables-restore -w --noflush <"$1/ipt6" && _cpxy_ipt_jumps
}

# cpxy_ipt_apply <rendered dir>: replaces each instance's set contents atomically (ipset swap),
# then the rules, then removes sets no instance uses. A failed set load leaves the previous policy.
cpxy_ipt_apply() {
	local dir="$1" set
	ipset -exist restore <"$dir/ipset" || return 1
	while read -r set; do
		ipset -exist create "$set" hash:net family inet hashsize 1024 maxelem 1048576 &&
			ipset swap "${set}_n" "$set" && ipset destroy "${set}_n" || return 1
	done <"$dir/sets"
	cpxy_ipt_load "$dir" || return 1
	mkdir -p "$CPXY_RUN" && cp "$dir/ipt4" "$dir/ipt6" "$CPXY_RUN/" || return 1
	_cpxy_ipt_stale_sets "$dir/sets"
}

# Restores the last applied rules after a firewall reload; nothing when none were applied.
cpxy_ipt_reapply() {
	[ -f "$CPXY_RUN/ipt4" ] && [ -f "$CPXY_RUN/ipt6" ] || return 0
	cpxy_ipt_load "$CPXY_RUN"
}

_cpxy_ipt_stale_sets() {
	local keep="$1" set
	for set in $(ipset list -n 2>/dev/null); do
		case "$set" in cpxy_*) ;; *) continue ;; esac
		grep -qxF "$set" "$keep" 2>/dev/null || ipset destroy "$set" 2>/dev/null
	done
	return 0
}

cpxy_ipt_clear() {
	local fam entry table builtin chain
	command -v iptables >/dev/null || return 0
	rm -f "$CPXY_RUN/ipt4" "$CPXY_RUN/ipt6"
	for fam in 4 6; do
		for entry in $CPXY_IPT_CHAINS; do
			IFS=: read -r table builtin chain <<EOF
$entry
EOF
			while _cpxy_ipt "$fam" -t "$table" -D "$builtin" -j "$chain" 2>/dev/null; do :; done
			_cpxy_ipt "$fam" -t "$table" -F "$chain" 2>/dev/null
			_cpxy_ipt "$fam" -t "$table" -X "$chain" 2>/dev/null
		done
	done
	command -v ipset >/dev/null && _cpxy_ipt_stale_sets /dev/null
	return 0
}

# Whether the installed rules guard forwarding from <device>
cpxy_ipt_guarded() {
	iptables -w -S cpxy_forward 2>/dev/null | grep -qF -- "--comment \"cpxy guard $1\""
}
