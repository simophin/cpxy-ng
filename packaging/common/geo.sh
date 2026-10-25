#!/bin/sh
# IPv4 GeoIP route generations. net.sh supplies selection and priority-scoped deletion.
# The terminal guards are independent of both tables. A missing/partial table never uses WAN.

cpxy_geo_guard() {
	local dev fam
	for dev in "$@"; do
		for fam in -4 -6; do
			ip "$fam" rule show priority "$((CPXY_PRIO_MAIN + 4))" | grep -qF "iif $dev " ||
				ip "$fam" rule add priority "$((CPXY_PRIO_MAIN + 4))" iif "$dev" prohibit || return 1
		done
	done
}

# Discover the simple effective main-table uplink. Reject ambiguous/multipath defaults.
# A missing uplink is valid: direct destinations receive unreachable routes until it returns.
cpxy_geo_uplink() {
	ip -4 route show table main default | awk '
		/^default / {
			if (++n > 1 || /nexthop/ || /encap/) exit 1
			for (i=1; i<=NF; i++) {
				if ($i == "via") gateway=$(i+1)
				if ($i == "dev") device=$(i+1)
			}
		}
		END {
			if (n > 1) exit 1
			if (n == 0) print "- -"
			else if (device ~ /^[A-Za-z0-9_.:@-]+$/ && (gateway == "" || gateway ~ /^[0-9.]+$/))
				print (gateway == "" ? "-" : gateway), device
			else exit 1
		}'
}

cpxy_geo_healthy() {
	local active actual expected copies=1 wan="$2"
	[ "$wan" = - ] || copies=2
	active="$(ip -4 rule show priority "$CPXY_PRIO_TUN" | awk '/lookup/ {print $NF; exit}')"
	case "$active" in "$CPXY_TABLE" | "$((CPXY_TABLE + 10000))") ;; *) return 1 ;; esac
	expected="$(wc -l <"$1")"
	actual="$(ip -4 route show table "$active" | wc -l)"
	[ "$actual" -eq "$((expected * copies + 2))" ] || return 1
	ip -4 route show table "$active" default | grep -q 'unreachable default' || return 1
	ip -4 route show table "$active" default | grep -q "default dev $CPXY_TUN " || return 1
	shift 2
	local dev fam
	for dev in "$CPXY_TUN" "$@"; do
		for fam in -4 -6; do
			ip "$fam" rule show priority "$((CPXY_PRIO_MAIN + 4))" | grep -qF "iif $dev " || return 1
		done
	done
	for dev in "$@"; do
		ip -4 rule show priority "$CPXY_PRIO_TUN" | grep -qF "iif $dev lookup $active" || return 1
		ip -4 rule show priority "$CPXY_PRIO_QUIC" | grep -qF "iif $dev ipproto udp dport 443 prohibit" || return 1
		ip -4 rule show priority "$CPXY_PRIO_QUIC" | grep -qF "iif $dev ipproto tcp dport 853 prohibit" || return 1
	done
}

# cpxy_geo_up <CIDR file> <gateway or -> <WAN device or -> <LAN devices...>
cpxy_geo_up() (
	local prefixes="$1" gateway="$2" wan="$3" dev fam active stage work count actual copies=1
	shift 3
	[ -f "$prefixes" ] || return 1
	cpxy_geo_guard "$CPXY_TUN" "$@" || return 1
	active="$(ip -4 rule show priority "$CPXY_PRIO_TUN" | awk '/lookup/ {print $NF; exit}')"
	case "$active" in '' | "$CPXY_TABLE" | "$((CPXY_TABLE + 10000))") ;; *) return 1 ;; esac
	stage="$CPXY_TABLE"
	[ "$active" != "$CPXY_TABLE" ] || stage="$((CPXY_TABLE + 10000))"
	work="$(mktemp -d /tmp/cpxy-geo.XXXXXX)" || return 1
	trap 'rm -rf "$work"' EXIT
	ip link show "$CPXY_TUN" >/dev/null 2>&1 || ip tuntap add dev "$CPXY_TUN" mode tun || return 1
	ip link set "$CPXY_TUN" up || return 1
	ip -4 route flush table "$stage" 2>/dev/null ||
		[ -z "$(ip -4 route show table "$stage" 2>/dev/null)" ] || return 1
	ip -4 route add unreachable default table "$stage" metric 1000 || return 1
	ip -4 route add default dev "$CPXY_TUN" table "$stage" metric 10 || return 1
	awk -v table="$stage" -v gateway="$gateway" -v wan="$wan" '
		{
			if ($0 !~ /^[0-9.]+\/[0-9]+$/) exit 1
			# Device-bound routes disappear on WAN teardown; the backup must remain.
			print "route add unreachable", $0, "metric 1000 table", table
			if (wan != "-") {
				if (gateway == "-") print "route add", $0, "dev", wan, "metric 10 table", table
				else print "route add", $0, "via", gateway, "dev", wan, "onlink metric 10 table", table
			}
		}' "$prefixes" >"$work/batch" || return 1
	ip -4 -batch "$work/batch" || return 1
	count="$(wc -l <"$prefixes")"
	[ "$wan" = - ] || copies=2
	actual="$(ip -4 route show table "$stage" | wc -l)"
	[ "$actual" -eq "$((count * copies + 2))" ] || return 1
	# Stage is complete. Remove selectors while the independent guards remain installed.
	for fam in -4 -6; do
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_MAIN"
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_QUIC"
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_UDP"
		_cpxy_flush_rules "$fam" "$CPXY_PRIO_TUN"
	done
	for dev in "$@"; do
		ip -4 rule add priority "$CPXY_PRIO_MAIN" iif "$dev" lookup main suppress_prefixlength 0 || return 1
		ip -4 rule add priority "$CPXY_PRIO_QUIC" iif "$dev" ipproto udp dport 443 prohibit || return 1
		ip -4 rule add priority "$CPXY_PRIO_QUIC" iif "$dev" ipproto tcp dport 853 prohibit || return 1
		ip -4 rule add priority "$CPXY_PRIO_UDP" iif "$dev" ipproto udp lookup main || return 1
		ip -4 rule add priority "$CPXY_PRIO_TUN" iif "$dev" lookup "$stage" || return 1
	done
	# The worker's TUN writes are replies, never a fresh GeoIP decision.
	ip -4 rule add priority "$CPXY_PRIO_MAIN" iif "$CPXY_TUN" lookup main suppress_prefixlength 0 || return 1
	# IPv6 has only the terminal LAN guard: local/router and on-link traffic stays local.
	if [ -n "$active" ] && [ "$active" != "$stage" ]; then
		ip -4 route flush table "$active" || return 1
	fi
	# A removed LAN must regain its ordinary routing after the replacement is active.
	for fam in -4 -6; do
		for dev in $(ip "$fam" rule show priority "$((CPXY_PRIO_MAIN + 4))" | awk '{for(i=1;i<NF;i++) if($i=="iif") print $(i+1)}'); do
			case " $CPXY_TUN $* " in *" $dev "*) ;; *)
				ip "$fam" rule del priority "$((CPXY_PRIO_MAIN + 4))" iif "$dev" prohibit || return 1 ;;
			esac
		done
	done
)

cpxy_geo_down() {
	local fam
	_cpxy_net_down_rules
	for fam in -4 -6; do
		_cpxy_flush_rules "$fam" "$((CPXY_PRIO_MAIN + 4))"
	done
	ip -4 route flush table "$((CPXY_TABLE + 10000))" 2>/dev/null
	ip link del "$CPXY_TUN" 2>/dev/null
	return 0
}
