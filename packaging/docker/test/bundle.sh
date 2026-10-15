#!/bin/sh
# End-to-end test of the bundle image against a real cpxy server on this machine:
#   - two containers side by side on host network, with their own ports and state
#   - one on a bridge network with its proxy published
#   - the proxy and tsgw reach the server, tsgw's Tailscale traffic through the proxy, and tsgw
#     sets up its alternative DNS through the server
#   - a service that dies is restarted alone; the container stops cleanly
#   - bad settings stop the container with a message
# Tailscale login itself is not tested: the nodes stop at their login links.
#
# usage: bundle.sh <image> <cpxy server binary>
# Needs docker and curl.
set -eu

[ $# -eq 2 ] || { sed -n '2,12p' "$0" >&2; exit 2; }
image="$1"
server_bin="$2"
key=bundle-test
work="$(mktemp -d)"
server_pid=

cleanup() {
	status=$?
	if [ "$status" != 0 ]; then
		for c in cpxy-test-a cpxy-test-b cpxy-test-bridge; do
			echo "== docker logs $c" >&2
			docker logs "$c" 2>&1 | tail -40 >&2 || true
		done
		echo "== server log" >&2
		tail -40 "$work/server.log" >&2 || true
	fi
	docker rm -f cpxy-test-a cpxy-test-b cpxy-test-bridge >/dev/null 2>&1 || true
	docker volume rm cpxy-test-a cpxy-test-b cpxy-test-bridge >/dev/null 2>&1 || true
	[ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null || true
	rm -rf "$work"
	exit "$status"
}
trap cleanup EXIT

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

# eventually <seconds> <description> <command...>
eventually() {
	local deadline=$(($(date +%s) + $1)) what="$2"
	shift 2
	until "$@" >/dev/null 2>&1; do
		[ "$(date +%s)" -lt "$deadline" ] || fail "timed out waiting for: $what"
		sleep 1
	done
}

server_saw() {
	grep -q "$1" "$work/server.log"
}

healthy() {
	[ "$(docker inspect -f '{{.State.Health.Status}}' "$1")" = healthy ]
}

pid_of() {
	docker exec "$1" pidof "$2"
}

NO_COLOR=1 RUST_LOG=info KEY="$key" BIND_ADDR=0.0.0.0:3000 "$server_bin" >"$work/server.log" 2>&1 &
server_pid=$!
eventually 10 "the cpxy server" grep -q "Server listening" "$work/server.log"

echo "== settings the container refuses"
if docker run --rm "$image" >"$work/refused.log" 2>&1; then
	fail "started without SERVER"
fi
grep -q "SERVER is required" "$work/refused.log" || { cat "$work/refused.log" >&2; fail "no reason given"; }
if docker run --rm -e SERVER=http://:x@127.0.0.1:3000 -e HTTP_PROXY_LISTEN=5000 "$image" >"$work/refused.log" 2>&1; then
	fail "started with a proxy address without a host"
fi
grep -q "HTTP_PROXY_LISTEN must be host:port" "$work/refused.log" || { cat "$work/refused.log" >&2; fail "no reason given"; }

echo "== two containers on host network"
docker run -d --name cpxy-test-a --network host -v cpxy-test-a:/var/lib/cpxy-tsgw \
	-e SERVER="http://:$key@127.0.0.1:3000" -e TS_HOSTNAME=cpxy-test-a \
	"$image" >/dev/null
docker run -d --name cpxy-test-b --network host -v cpxy-test-b:/var/lib/cpxy-tsgw \
	-e SERVER="http://:$key@127.0.0.1:3000" -e TS_HOSTNAME=cpxy-test-b \
	-e HTTP_PROXY_LISTEN='[::]:5001' -e SOCKS5_PROXY_LISTEN='[::]:1081' \
	"$image" >/dev/null
eventually 60 "cpxy-test-a healthy" healthy cpxy-test-a
eventually 60 "cpxy-test-b healthy" healthy cpxy-test-b

curl -fsS -o /dev/null -x http://127.0.0.1:5000 https://example.com || fail "a's proxy"
curl -fsS -o /dev/null -x http://127.0.0.1:5001 https://example.com || fail "b's proxy"
curl -fsS -o /dev/null --socks5-hostname 127.0.0.1:1081 https://example.com || fail "b's SOCKS5 proxy"
server_saw 'target_host="example.com"' || fail "the proxies did not go through the server"
echo "proxies: ok"

eventually 60 "tsgw reaching Tailscale through the proxy" server_saw 'target_host="controlplane.tailscale.com"'
eventually 60 "tsgw's login link" sh -c "docker logs cpxy-test-a 2>&1 | grep -q '^\[tsgw\].*https://login.tailscale.com/'"
env="$(docker exec cpxy-test-b sh -c 'tr "\0" "\n" </proc/$(pidof cpxy-tsgw)/environ')"
echo "$env" | grep -qx 'DNS_ALTERNATIVE=tcp://1.1.1.1?via=cpxy' ||
	fail "b's tsgw does not ask its alternative DNS through the server"
docker logs cpxy-test-a 2>&1 | grep -q '^\[tsgw\].*Using alternative DNS server tcp://1.1.1.1:53?via=cpxy' ||
	fail "a's tsgw did not set up its alternative DNS through the server"
echo "$env" | grep -qx 'HTTPS_PROXY=http://127.0.0.1:5001' || fail "b's tsgw does not use b's proxy"
echo "$env" | grep -qx 'TS_HOSTNAME=cpxy-test-b' || fail "b's tsgw has the wrong name"
docker logs cpxy-test-a 2>&1 | grep -q '^\[client\]' || fail "the client's log lines are not prefixed"
echo "tsgw: ok"

echo "== a service that dies is restarted alone"
client="$(pid_of cpxy-test-a cpxy-client)"
tsgw="$(pid_of cpxy-test-a cpxy-tsgw)"
docker exec cpxy-test-a kill -9 "$client"
eventually 10 "the client back" sh -c "p=\$(docker exec cpxy-test-a pidof cpxy-client) && [ \"\$p\" != $client ]"
[ "$(pid_of cpxy-test-a cpxy-tsgw)" = "$tsgw" ] || fail "tsgw restarted too"
eventually 10 "the proxy answering again" curl -fsS -o /dev/null -x http://127.0.0.1:5000 https://example.com
echo "restart: ok"

echo "== a container on a bridge network, proxy published"
docker run -d --name cpxy-test-bridge -p 127.0.0.1:15000:5000 -v cpxy-test-bridge:/var/lib/cpxy-tsgw \
	--add-host=host.docker.internal:host-gateway \
	-e SERVER="http://:$key@host.docker.internal:3000" "$image" >/dev/null
eventually 60 "cpxy-test-bridge healthy" healthy cpxy-test-bridge
curl -fsS -o /dev/null -x http://127.0.0.1:15000 https://example.com || fail "the published proxy"
echo "bridge: ok"

echo "== state is kept, and stopping is clean"
docker exec cpxy-test-a sh -c 'ls /var/lib/cpxy-tsgw' | grep -q . || fail "no tsgw state in the volume"
start=$(date +%s)
docker stop cpxy-test-a >/dev/null
[ $(($(date +%s) - start)) -lt 10 ] || fail "docker stop had to kill the container"
[ "$(docker inspect -f '{{.State.ExitCode}}' cpxy-test-a)" = 0 ] || fail "exit code $(docker inspect -f '{{.State.ExitCode}}' cpxy-test-a)"
docker start cpxy-test-a >/dev/null
eventually 60 "cpxy-test-a healthy again" healthy cpxy-test-a
echo "stop and start: ok"

echo "bundle image checks passed"
