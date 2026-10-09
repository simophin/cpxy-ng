#!/bin/sh
# Runs dns_split for procd and points dnsmasq at it only while it is listening, so the LAN's DNS is
# never forwarded to a resolver that failed to start (a port already taken, a bad server option).
# When dns_split exits, dnsmasq goes back to its own servers until procd starts it again.
#
# usage: dns-split.sh <listen host:port> <dns_split command...>
. /usr/libexec/cpxy/dnsmasq.sh

listen="$1"
shift
port="$(printf '%04X' "${listen##*:}")"

# A socket bound to the port: UDP in any state, TCP in LISTEN (0A). Columns: sl local remote state
_cpxy_bound() {
	awk -v port=":$port" -v proto="$1" \
		'substr($2, length($2) - 4) == port && (proto == "udp" || $4 == "0A") { found = 1 } END { exit !found }' \
		"/proc/net/$1" "/proc/net/${1}6" 2>/dev/null
}

"$@" &
pid=$!
trap 'kill "$pid" 2>/dev/null' TERM INT

# dns_split binds UDP then TCP, after setting up its servers (which can take a few seconds)
while kill -0 "$pid" 2>/dev/null; do
	if _cpxy_bound udp && _cpxy_bound tcp; then
		cpxy_dnsmasq_up "$listen"
		break
	fi
	sleep 1
done

wait "$pid"
status=$?
# `wait` returns early when a trapped signal arrives; reap the child before handing DNS back
kill -0 "$pid" 2>/dev/null && wait "$pid"
cpxy_dnsmasq_down
exit "$status"
