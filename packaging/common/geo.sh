#!/bin/sh
# Dual-family GeoIP route generations. net.sh supplies selection and priority-scoped deletion.
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
	local family="${1:--4}"
	ip "$family" route show table main default | awk '
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
			else if (device ~ /^[A-Za-z0-9_.:@-]+$/ && (gateway == "" || gateway ~ /^[0-9a-fA-F:.]+$/))
				print (gateway == "" ? "-" : gateway), device
			else exit 1
		}'
}

# CPXY_GEO6_ROUTES/GATEWAY/WAN opt into a second-family generation. Legacy callers
# without these variables retain IPv4-only behavior; the router service always supplies them.
cpxy_geo_healthy() {
    local routes="$1" wan="$2" family dev active expected actual copies devices
    shift 2
    devices="$*"
    for family in -4 -6; do
        [ "$family" != -6 ] || [ -n "${CPXY_GEO6_ROUTES:-}" ] || continue
        if [ "$family" = -6 ]; then routes="$CPXY_GEO6_ROUTES"; wan="$CPXY_GEO6_WAN"; fi
        copies=1; [ "$wan" = - ] || copies=2
        active="$(ip "$family" rule show priority "$CPXY_PRIO_TUN" | awk '/lookup/ {print $NF; exit}')"
        case "$active" in "$CPXY_TABLE" | "$((CPXY_TABLE + 10000))") ;; *) return 1 ;; esac
        expected="$(wc -l <"$routes")"
        actual="$(ip "$family" route show table "$active" | wc -l)"
        [ "$actual" -eq "$((expected * copies + 2))" ] || return 1
        ip "$family" route show table "$active" default | grep -q 'unreachable default' || return 1
        ip "$family" route show table "$active" default | grep -q "default dev $CPXY_TUN " || return 1
        for dev in $devices; do
            ip "$family" rule show priority "$CPXY_PRIO_TUN" | grep -qF "iif $dev ipproto tcp lookup $active" || return 1
            ip "$family" rule show priority "$CPXY_PRIO_MAIN" | grep -qF "iif $dev ipproto udp dport 443 prohibit" || return 1
            ip "$family" rule show priority "$CPXY_PRIO_UDP" | grep -qF "iif $dev ipproto udp lookup main" || return 1
        done
    done
    for dev in "$CPXY_TUN" $devices; do
        for family in -4 -6; do
            ip "$family" rule show priority "$((CPXY_PRIO_MAIN + 4))" | grep -qF "iif $dev " || return 1
        done
    done
}

# Stage a complete family before touching its selectors. Device-bound direct routes
# have unreachable backups so WAN teardown never changes CN traffic into proxy traffic.
_cpxy_geo_stage() {
    local family="$1" prefixes="$2" gateway="$3" wan="$4" stage="$5" work="$6" count actual copies=1
    [ -f "$prefixes" ] || return 1
    ip "$family" route flush table "$stage" 2>/dev/null ||
        [ -z "$(ip "$family" route show table "$stage" 2>/dev/null)" ] || return 1
    ip "$family" route add unreachable default table "$stage" metric 1000 || return 1
    ip "$family" route add default dev "$CPXY_TUN" table "$stage" metric 10 || return 1
    awk -v table="$stage" -v gateway="$gateway" -v wan="$wan" '
        {
            if ($0 !~ /^[0-9a-fA-F:.]+\/[0-9]+$/) exit 1
            print "route add unreachable", $0, "metric 1000 table", table
            if (wan != "-") {
                if (gateway == "-") print "route add", $0, "dev", wan, "metric 10 table", table
                else print "route add", $0, "via", gateway, "dev", wan, "onlink metric 10 table", table
            }
        }' "$prefixes" >"$work/batch" || return 1
    # Older iproute2 needs an open stdin when procd inherits a closed descriptor 0.
    ip "$family" -batch "$work/batch" </dev/null || return 1
    count="$(wc -l <"$prefixes")"; [ "$wan" = - ] || copies=2
    actual="$(ip "$family" route show table "$stage" | wc -l)"
    [ "$actual" -eq "$((count * copies + 2))" ]
}

# cpxy_geo_up <IPv4 CIDR file> <IPv4 gateway or -> <IPv4 WAN or -> <LAN devices...>
cpxy_geo_up() (
    local prefixes="$1" gateway="$2" wan="$3" dev fam active stage work active6 families=-4
    shift 3
    cpxy_geo_guard "$CPXY_TUN" "$@" || return 1
    active="$(ip -4 rule show priority "$CPXY_PRIO_TUN" | awk '/lookup/ {print $NF; exit}')"
    case "$active" in '' | "$CPXY_TABLE" | "$((CPXY_TABLE + 10000))") ;; *) return 1 ;; esac
    if [ -n "${CPXY_GEO6_ROUTES:-}" ]; then
        active6="$(ip -6 rule show priority "$CPXY_PRIO_TUN" | awk '/lookup/ {print $NF; exit}')"
        case "$active6" in '' | "$CPXY_TABLE" | "$((CPXY_TABLE + 10000))") ;; *) return 1 ;; esac
        if [ "$active6" != "$active" ]; then
            # Recover an interrupted family switch under the terminal guards. Neither
            # selected table may be overwritten while it can still receive traffic.
            for fam in -4 -6; do
                _cpxy_flush_rules "$fam" "$CPXY_PRIO_MAIN"
                _cpxy_flush_rules "$fam" "$CPXY_PRIO_QUIC"
                _cpxy_flush_rules "$fam" "$CPXY_PRIO_UDP"
                _cpxy_flush_rules "$fam" "$CPXY_PRIO_TUN"
            done
            active=''
        fi
    fi
    stage="$CPXY_TABLE"; [ "$active" != "$CPXY_TABLE" ] || stage="$((CPXY_TABLE + 10000))"
    work="$(mktemp -d /tmp/cpxy-geo.XXXXXX)" || return 1
    trap 'rm -rf "$work"' EXIT
    ip link show "$CPXY_TUN" >/dev/null 2>&1 || ip tuntap add dev "$CPXY_TUN" mode tun || return 1
    ip link set "$CPXY_TUN" up || return 1
    _cpxy_geo_stage -4 "$prefixes" "$gateway" "$wan" "$stage" "$work" || return 1
    if [ -n "${CPXY_GEO6_ROUTES:-}" ]; then
        _cpxy_geo_stage -6 "$CPXY_GEO6_ROUTES" "$CPXY_GEO6_GATEWAY" "$CPXY_GEO6_WAN" "$stage" "$work" || return 1
        families="-4 -6"
    fi
    for fam in -4 -6; do
        _cpxy_flush_rules "$fam" "$CPXY_PRIO_MAIN"
        _cpxy_flush_rules "$fam" "$CPXY_PRIO_QUIC"
        _cpxy_flush_rules "$fam" "$CPXY_PRIO_UDP"
        _cpxy_flush_rules "$fam" "$CPXY_PRIO_TUN"
    done
    for fam in $families; do
        for dev in "$@"; do
            # Block QUIC even when a connected route would bypass the split table.
            ip "$fam" rule add priority "$CPXY_PRIO_MAIN" iif "$dev" ipproto udp dport 443 prohibit || return 1
            ip "$fam" rule add priority "$CPXY_PRIO_QUIC" iif "$dev" lookup main suppress_prefixlength 0 || return 1
            ip "$fam" rule add priority "$CPXY_PRIO_UDP" iif "$dev" ipproto udp lookup main || return 1
            ip "$fam" rule add priority "$CPXY_PRIO_TUN" iif "$dev" ipproto tcp lookup "$stage" || return 1
        done
        # TUN writes are replies; never reclassify them through the proxy.
        ip "$fam" rule add priority "$CPXY_PRIO_MAIN" iif "$CPXY_TUN" lookup main suppress_prefixlength 0 || return 1
        active="$CPXY_TABLE"; [ "$stage" != "$CPXY_TABLE" ] || active="$((CPXY_TABLE + 10000))"
        ip "$fam" route flush table "$active" 2>/dev/null ||
            [ -z "$(ip "$fam" route show table "$active" 2>/dev/null)" ] || return 1
    done
    for fam in -4 -6; do
        for dev in $(ip "$fam" rule show priority "$((CPXY_PRIO_MAIN + 4))" | awk '{for(i=1;i<NF;i++) if($i=="iif") print $(i+1)}'); do
            case " $CPXY_TUN $* " in *" $dev "*) ;; *)
                ip "$fam" rule del priority "$((CPXY_PRIO_MAIN + 4))" iif "$dev" prohibit || return 1 ;;
            esac
        done
    done
)

cpxy_geo_down() {
	local fam prio
	for fam in -4 -6; do
		for prio in "$CPXY_PRIO_MAIN" "$CPXY_PRIO_QUIC" "$CPXY_PRIO_UDP" "$CPXY_PRIO_TUN" "$((CPXY_PRIO_MAIN + 4))"; do
			_cpxy_flush_rules "$fam" "$prio"
		done
	done
	for fam in -4 -6; do
		ip "$fam" route flush table "$CPXY_TABLE" 2>/dev/null
		ip "$fam" route flush table "$((CPXY_TABLE + 10000))" 2>/dev/null
	done
	ip link del "$CPXY_TUN" 2>/dev/null
	return 0
}
