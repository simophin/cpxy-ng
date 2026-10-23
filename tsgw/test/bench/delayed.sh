#!/bin/sh
# Runs the benchmark with a round trip of twice <delay> between the client node and the gateway,
# as between a phone and an exit node, in a network namespace of its own: no root or tailnet
# needed, only unprivileged user namespaces, iproute2 and the kernel's netem. Only UDP (WireGuard
# between the nodes) is delayed; the gateway's own TCP leg to the server is not.
#
# usage: delayed.sh <delay, e.g. 10ms> [loss, e.g. 1%] [go test arguments]
#   e.g. CPXY_TSGW_BENCH_MB=50 bench/delayed.sh 10ms -run TestThroughput
# NETEM_OFFLOADS=off disables loopback TCP/UDP offloads (needs ethtool).
# The default preserves host offloads for comparison with historical results.
# Netem drops skbs, which may contain multiple datagrams when UDP GSO is active.
set -eu
delay="$1"
shift
loss="${LOSS:-0%}"
case "${1:-}" in
	*%) loss="$1"; shift ;;
esac
case "${NETEM_OFFLOADS:-preserve}" in
	preserve|off) ;;
	*) echo 'NETEM_OFFLOADS must be preserve or off' >&2; exit 2 ;;
esac
cd "$(dirname "$0")/.."
export CPXY_TSGW_BENCH=1 CPXY_TSGW_BENCH_HOST=10.9.9.9 DELAY="$delay" LOSS="$loss" NETEM_OFFLOADS="${NETEM_OFFLOADS:-preserve}"
exec unshare --map-root-user --net sh -euc '
	ip link set lo up
	if test "$NETEM_OFFLOADS" = off; then
		ethtool -K lo tso off gso off gro off tx-udp-segmentation off >&2
		ethtool -k lo >&2
	fi
	tc qdisc add dev lo root handle 1: prio
	tc qdisc add dev lo parent 1:3 handle 30: netem delay "$DELAY" loss "$LOSS" limit 100000
	tc filter add dev lo parent 1: protocol ip u32 match ip protocol 17 0xff flowid 1:3
	# An address of the host for the server, the gateway and the client to use.
	ip link add bench0 type dummy
	ip addr add 10.9.9.9/32 dev bench0
	ip link set bench0 up
	exec go test -v -count=1 -timeout 30m ./bench "$@"
' sh "$@"
