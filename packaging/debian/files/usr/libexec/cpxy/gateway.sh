#!/bin/sh
# Policy routing for the forwarding interfaces, and the packet engine.
#
#   gateway.sh up     install the TUN and policy routing (idempotent)
#   gateway.sh run    run the packet engine on the TUN, restarting it whenever it dies
#   gateway.sh down   remove the TUN and routing, restoring direct forwarding
#
# While the engine restarts, the routing stays, so forwarded traffic is refused rather than leaked.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
. "$here/config.sh"
. "$here/net.sh"
CPXY_BIN="${CPXY_BIN:-/usr/bin}"

cpxy_load_config
cpxy_net_select "$CPXY_TUN" "$CPXY_ROUTING_TABLE" "$CPXY_RULE_PRIORITY"

_warn_forwarding() {
	[ "$(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null)" = 1 ] ||
		cpxy_log "warning: net.ipv4.ip_forward is 0, so nothing is forwarded (see /usr/lib/sysctl.d/60-cpxy.conf)"
	local dev mtu
	for dev in $CPXY_INTERFACES; do
		mtu="$(cat "/sys/class/net/$dev/mtu" 2>/dev/null)" || continue
		[ "$mtu" -ge "$CPXY_MTU" ] ||
			cpxy_log "warning: $dev's MTU is $mtu but MTU=$CPXY_MTU; large replies will stall (lower MTU)"
	done
}

case "${1:-}" in
	up)
		cpxy_check_routing_config
		_warn_forwarding
		# shellcheck disable=SC2086 # one argument per interface
		cpxy_net_up $CPXY_INTERFACES || cpxy_die "could not set up policy routing"
		if [ -n "$CPXY_TUN_ADDRESS" ]; then
			ip addr replace "$CPXY_TUN_ADDRESS" dev "$CPXY_TUN"
		fi
		ip link set "$CPXY_TUN" mtu "$CPXY_MTU"
		;;
	run)
		cpxy_check_routing_config
		export SERVER="$CPXY_SERVER"
		child=""
		trap '[ -z "$child" ] || kill "$child" 2>/dev/null; exit 0' TERM INT
		while :; do
			"$CPXY_BIN/cpxy-router" --tun "$CPXY_TUN" --mtu "$CPXY_MTU" &
			child=$!
			status=0
			wait "$child" || status=$?
			cpxy_log "engine exited ($status); restarting in 2s, forwarded traffic is refused meanwhile"
			# In the background so a stop does not wait out the delay
			sleep 2 &
			child=$!
			wait "$child" || :
		done
		;;
	down)
		cpxy_net_down_routing
		;;
	*)
		echo "usage: $0 up|run|down" >&2
		exit 2
		;;
esac
