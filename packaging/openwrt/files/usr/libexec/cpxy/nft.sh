#!/bin/sh
# The kernel side of routed instances: one nftables table, `inet cpxy`, rebuilt as a whole in one
# `nft -f` transaction. Per instance, for packets arriving on its networks' devices:
#
#   IPv4 TCP to a direct address (CN, private, forced routes excluded, main-table routes)
#       -> forwarded by the kernel, through the firewall's own LAN-to-WAN forwarding
#   other IPv4 TCP (not to the router itself, not DNS)
#       -> redirected to the instance's cpxy-client transparent listener
#   IPv4 UDP/443 forwarded -> rejected, so QUIC falls back to TCP
#   IPv6 forwarded -> rejected; routed clients are IPv4-only
#   other IPv4 UDP and ICMP -> forwarded directly
#
# The forward guard rejects any non-direct TCP that would be forwarded, so if the redirect is
# missing proxied TCP is refused instead of leaving through the WAN. Redirected packets are
# input, not forward, and never reach it. Nothing the router originates is touched.
#
# A plan directory holds a state file per routed instance (see cpxy_nft_state) and its direct
# IPv4 prefixes in routes/<instance>.
# The variables cpxy_nft_read sets are for the scripts that source this.
# shellcheck disable=SC2034

CPXY_NFT_TABLE='inet cpxy'

# cpxy_nft_state <file> <listener port> <dns port or -> <policy hash> <zones> <devices...>
cpxy_nft_state() {
	local file="$1" port="$2" dns="$3" hash="$4" zones="$5"
	shift 5
	printf '%s\n' nft "$port" "$*" "$zones" "$dns" "$hash" >"$file"
}

# cpxy_nft_read <state file>: sets PORT, LAN_DEVS, ZONES, DNS_PORT and POLICY_HASH. Fails for
# state from before the nftables design (see cpxy_legacy_down).
cpxy_nft_read() {
	local kind
	{
		read -r kind
		read -r PORT
		read -r LAN_DEVS
		read -r ZONES
		read -r DNS_PORT
		read -r POLICY_HASH
	} <"$1"
	[ "$kind" = nft ]
}

# Device names go into nft strings unquoted otherwise; refuse anything unusual.
_cpxy_nft_dev_ok() {
	case "$1" in '' | *[!A-Za-z0-9_.:@-]*) return 1 ;; esac
}

# cpxy_nft_render <plan dir> [exempt devices...]: prints the ruleset that replaces the table, or
# deletes it when the plan has no routed instance. TCP whose route leaves through an exempt device
# (a tunnel such as tailscale0, another LAN) is neither redirected nor guarded: like the TUN
# design's `lookup main suppress_prefixlength 0`, such routes win over the proxy.
cpxy_nft_render() (
	local plan="$1" state section dev set any=0 exempt="" via="" out=""
	shift
	for dev in "$@"; do
		_cpxy_nft_dev_ok "$dev" || return 1
		exempt="${exempt:+$exempt, }\"$dev\""
	done
	if [ -n "$exempt" ]; then
		via=" fib daddr oif != @exempt"
		out=" oif != @exempt"
	fi
	echo "table $CPXY_NFT_TABLE"
	echo "delete table $CPXY_NFT_TABLE"
	for state in "$plan"/*; do [ ! -f "$state" ] || any=1; done
	[ "$any" = 1 ] || return 0
	echo "table $CPXY_NFT_TABLE {"
	[ -z "$exempt" ] || printf '\tset exempt {\n\t\ttype iface_index\n\t\telements = { %s }\n\t}\n' "$exempt"
	for state in "$plan"/*; do
		[ -f "$state" ] || continue
		section="${state##*/}"
		cpxy_nft_read "$state" || return 1
		printf '\tset direct_%s {\n\t\ttype ipv4_addr\n\t\tflags interval\n\t\tauto-merge\n' "$section"
		awk '
			$0 !~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(\/[0-9]+)?$/ { bad = 1; exit 1 }
			{ printf "%s\t\t\t%s", (NR == 1 ? "\t\telements = {\n" : ",\n"), $0 }
			END { if (!bad && NR > 0) printf "\n\t\t}\n" }
		' "$plan/routes/$section" || return 1
		printf '\t}\n'
	done
	printf '\tchain dstnat {\n\t\ttype nat hook prerouting priority dstnat + 5; policy accept;\n'
	for state in "$plan"/*; do
		[ -f "$state" ] || continue
		section="${state##*/}"
		cpxy_nft_read "$state"
		set="@direct_$section"
		for dev in $LAN_DEVS; do
			_cpxy_nft_dev_ok "$dev" || return 1
			# After fw4's DNAT (priority dstnat), so its DNS redirect wins for TCP/53
			printf '\t\tiifname "%s" meta nfproto ipv4 tcp dport != 53 fib daddr type != { local, broadcast, multicast } ip daddr != %s%s counter redirect to :%s comment "cpxy redirect %s"\n' \
				"$dev" "$set" "$via" "$PORT" "$dev"
		done
	done
	printf '\t}\n'
	printf '\tchain forward {\n\t\ttype filter hook forward priority filter - 5; policy accept;\n'
	for state in "$plan"/*; do
		[ -f "$state" ] || continue
		section="${state##*/}"
		cpxy_nft_read "$state"
		set="@direct_$section"
		for dev in $LAN_DEVS; do
			printf '\t\tiifname "%s" meta nfproto ipv6 counter reject comment "cpxy ipv6 %s"\n' "$dev" "$dev"
			printf '\t\tiifname "%s" meta nfproto ipv4 udp dport 443 counter reject comment "cpxy quic %s"\n' "$dev" "$dev"
			printf '\t\tiifname "%s" meta nfproto ipv4 meta l4proto tcp ip daddr != %s%s counter reject with tcp reset comment "cpxy guard %s"\n' \
				"$dev" "$set" "$out" "$dev"
		done
	done
	printf '\t}\n}\n'
)

# cpxy_nft_apply <ruleset file>
cpxy_nft_apply() {
	nft -f "$1"
}

cpxy_nft_clear() {
	nft delete table $CPXY_NFT_TABLE 2>/dev/null
	return 0
}

# Whether the installed table guards forwarding from <device>
cpxy_nft_guarded() {
	nft list chain $CPXY_NFT_TABLE forward 2>/dev/null | grep -qF "comment \"cpxy guard $1\""
}

# The IPv4 prefixes of the main table's routes other than the default, which were direct under
# the TUN design's `lookup main suppress_prefixlength 0` (connected and static subnets).
cpxy_nft_main_routes() {
	ip -4 route show table main 2>/dev/null | awk '
		$1 == "default" || $1 == "unreachable" || $1 == "blackhole" || $1 == "prohibit" || $1 == "throw" { next }
		$1 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+(\/[0-9]+)?$/ { print $1 }'
}

# cpxy_nft_exempt <uplink devices...>: every network device of the router except loopback and the
# uplinks (where the internet is). The set holds interface indexes: a device created (or
# recreated) after this is not exempt, so new paths fail closed until the next apply.
cpxy_nft_exempt() {
	local dev
	for dev in /sys/class/net/*; do
		dev="${dev##*/}"
		case " lo $* " in *" $dev "*) continue ;; esac
		_cpxy_nft_dev_ok "$dev" && echo "$dev"
	done
	return 0
}

# cpxy_legacy_down <tun> <table> <priority>: removes what an instance of the TUN design left in
# the kernel: its device, its two routing tables and its five rule priorities.
cpxy_legacy_down() {
	local tun="$1" table="$2" prio="$3" fam n
	_cpxy_nft_dev_ok "$tun" || return 0
	case "$table$prio" in '' | *[!0-9]*) return 0 ;; esac
	for fam in -4 -6; do
		for n in 0 1 2 3 4; do
			while ip "$fam" rule del priority "$((prio + n))" 2>/dev/null; do :; done
		done
		ip "$fam" route flush table "$table" 2>/dev/null
		ip "$fam" route flush table "$((table + 10000))" 2>/dev/null
	done
	ip link del "$tun" 2>/dev/null
	return 0
}
