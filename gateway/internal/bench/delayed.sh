#!/bin/sh
# Runs the benchmark with a round trip of twice <delay> between the client node and the gateway,
# as between a phone and an exit node, in a network namespace of its own: no root or tailnet
# needed, only unprivileged user namespaces, iproute2 and the kernel's netem. Only UDP (WireGuard
# between the nodes) is delayed; the gateway's own TCP leg to the server is not.
#
# usage: delayed.sh <delay, e.g. 10ms> [go test arguments]
#   e.g. CPXY_TSGW_BENCH_MB=50 internal/bench/delayed.sh 10ms -run TestThroughput
set -eu
delay="$1"
shift
cd "$(dirname "$0")/../.."
export CPXY_TSGW_BENCH=1 CPXY_TSGW_BENCH_HOST=10.9.9.9 DELAY="$delay"
exec unshare --map-root-user --net sh -euc '
	ip link set lo up
	tc qdisc add dev lo root handle 1: prio
	tc qdisc add dev lo parent 1:3 handle 30: netem delay "$DELAY" limit 100000
	tc filter add dev lo parent 1: protocol ip u32 match ip protocol 17 0xff flowid 1:3
	# An address of the host for the server, the gateway and the client to use.
	ip link add bench0 type dummy
	ip addr add 10.9.9.9/32 dev bench0
	ip link set bench0 up
	exec go test -v -count=1 -timeout 30m ./internal/bench "$@"
' sh "$@"
