#!/bin/sh
# Kernel CUBIC reference for delayed.sh. Run as a regular user on Linux with
# unprivileged user/network namespaces, iproute2, ethtool and iperf3 installed.
# usage: kernel.sh <delay> <loss> <socket buffer> [seconds] [iperf3 client flags]
# e.g. kernel.sh 27ms 0.5% 256K 60 -R >kernel.json 2>kernel.log
#
# MTU 1280 matches the gateway. Disable aggregation so netem drops individual
# packets, as it does for WireGuard UDP in delayed.sh. Linux doubles SO_SNDBUF /
# SO_RCVBUF requests for bookkeeping; this is a reference, not an exact memory
# or end-to-end gateway comparison. JSON includes the actual buffer settings.
set -eu
export DELAY="$1" LOSS="$2" WINDOW="$3" BENCH_SECONDS="${4:-60}"
shift 3
if test "$#" -gt 0; then shift; fi
# Variables below are expanded inside the new namespace.
# shellcheck disable=SC2016
exec unshare --map-root-user --net sh -euc '
    ip link set lo up
    ip link set lo mtu 1280
    ethtool -K lo tso off gso off gro off >&2
    ethtool -k lo >&2
    tc qdisc add dev lo root netem delay "$DELAY" loss "$LOSS" limit 100000
    iperf3 -s -1 > /dev/null 2>&1 &
    server=$!
    trap '\''kill "$server" 2>/dev/null || true'\'' EXIT HUP INT TERM
    # Wait for the listener without consuming its one client connection.
    tries=0
    until ss -ltn sport = :5201 | grep -q LISTEN; do
        tries=$((tries + 1))
        test "$tries" -lt 50
        sleep 0.1
    done
    iperf3 -c 127.0.0.1 -C cubic -w "$WINDOW" -t "$BENCH_SECONDS" -J "$@"
    tc -s qdisc show dev lo >&2
    wait "$server"
' sh "$@"
