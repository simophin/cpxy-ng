#!/bin/sh
# Gateway lab: the .deb's routing scripts and the real binaries, in throwaway network namespaces
# (no root needed; uses an unprivileged user namespace). Stands in for a Tailscale exit node:
#
#   client (100.64.0.2) -- tailscale0 (MTU 1280) -- gateway (WAN 192.0.2.2) -- inet
#   inet: a cpxy server on 192.0.2.1:8443 and a web server on 93.184.216.34:8000 (proxied
#   requests reach it from 93.184.216.34, the server's address on that side)
#
# The gateway masquerades what it forwards from tailscale0, as Tailscale does, so the TUN needs
# an address. The client's link has Tailscale's 1280 MTU, so a too-large engine MTU stalls
# downloads. systemd is not involved: the lab runs gateway.sh up/run/down itself.
#
# usage: lab.sh <deb> <dir with cpxy-server>
set -u

if [ -z "${CPXY_LAB_INNER:-}" ]; then
	exec env CPXY_LAB_INNER=1 unshare -Urmn --propagation private sh "$0" "$@"
fi

DEB="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
BIN="$(cd "$2" && pwd)"
WORK="$(mktemp -d)"
FAILED=0
PIDS=""
cleanup() {
	for p in $PIDS $(for ns in inet gw client; do ip netns pids "$ns" 2>/dev/null; done); do kill "$p" 2>/dev/null; done
	rm -rf "$WORK"
}
trap cleanup EXIT

pass() { echo "ok   - $1"; }
fail() { echo "FAIL - $1"; FAILED=1; }
check() { # check <name> <cmd...>
	name="$1"; shift
	if "$@" >"$WORK/out" 2>&1; then pass "$name"; else fail "$name"; sed 's/^/       /' "$WORK/out"; fi
}
check_not() {
	name="$1"; shift
	if "$@" >"$WORK/out" 2>&1; then fail "$name"; sed 's/^/       /' "$WORK/out"; else pass "$name"; fi
}
bg() { "$@" & PIDS="$PIDS $!"; }

mkdir "$WORK/root"
(cd "$WORK" && ar x "$DEB" data.tar.xz) && tar -xJf "$WORK/data.tar.xz" -C "$WORK/root" ||
	{ echo "cannot unpack $DEB"; exit 1; }
GW="$WORK/root/usr/libexec/cpxy/gateway.sh"
export CPXY_BIN="$WORK/root/usr/bin" CPXY_CONF="$WORK/cpxy.conf"
cat >"$CPXY_CONF" <<'EOF'
SERVER=http://:lab@192.0.2.1:8443
INTERFACES=tailscale0
EOF

mount -t tmpfs tmpfs /run && mkdir -p /run/netns
for ns in inet gw client; do ip netns add "$ns"; done
ip link add wan0 type veth peer name wan0g
ip link set wan0 netns inet; ip link set wan0g netns gw
ip link add tailscale0 type veth peer name ts0
ip link set tailscale0 netns gw; ip link set ts0 netns client

in_inet() { ip netns exec inet "$@"; }
in_gw() { ip netns exec gw "$@"; }
in_client() { ip netns exec client "$@"; }

in_inet sh -c 'ip link set lo up; ip addr add 93.184.216.34/32 dev lo; ip addr add 114.114.114.114/32 dev lo
	ip addr add 192.0.2.1/24 dev wan0; ip link set wan0 up'
in_gw sh -c 'ip link set lo up; ip addr add 192.0.2.2/24 dev wan0g; ip link set wan0g up; ip route add default via 192.0.2.1
	ip addr add 100.72.0.1/32 dev tailscale0; ip link set tailscale0 mtu 1280 up; ip route add 100.64.0.2/32 dev tailscale0
	sysctl -qw net.ipv4.ip_forward=1'
in_client sh -c 'ip link set lo up; ip addr add 100.64.0.2/32 dev ts0; ip link set ts0 mtu 1280 up
	ip route add 100.72.0.1/32 dev ts0; ip route add default via 100.72.0.1 dev ts0'

# Tailscale's own rules, reduced: it masquerades whatever it forwards from tailscale0
cat >"$WORK/ts.nft" <<'EOF'
table ip ts {
	chain postrouting {
		type nat hook postrouting priority srcnat; policy accept;
		iifname "tailscale0" masquerade
	}
}
EOF
check "gateway firewall loads" in_gw nft -f "$WORK/ts.nft"

# --- Internet side ---
mkdir "$WORK/www" && echo hello >"$WORK/www/index.html"
# Several times the MTU, so a download needs full-sized segments
head -c 2000000 /dev/urandom >"$WORK/www/big.bin"
bg in_inet "$BIN/cpxy-server" --key lab 192.0.2.1:8443 >"$WORK/server.log" 2>&1
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 93.184.216.34 2>"$WORK/web.log")
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 114.114.114.114 2>"$WORK/direct.log")
sleep 1

URL=http://93.184.216.34:8000
fetch() { in_client curl -sS --max-time "${TIMEOUT:-10}" --noproxy '*' "$URL/${1:-index.html}"; }
fetch_big() { [ "$(fetch big.bin | cmp - "$WORK/www/big.bin" && echo same)" = same ]; }
mark() { MARK="$(wc -l <"$WORK/web.log")"; }
seen() { sleep 0.3; tail -n +"$((MARK + 1))" "$WORK/web.log" | grep -o '^[0-9.]*' | sort -u | tr '\n' ' ' | sed 's/ $//'; }
expect_seen() {
	got="$(seen)"
	if [ "$got" = "$2" ]; then pass "$1 (web server saw: ${got:-nothing})"; else fail "$1 (web server saw: '${got}', wanted '$2')"; fi
}
expect_not_direct() {
	got="$(seen)"
	case "$got" in *192.0.2.2*) fail "$1 (traffic left via the WAN)" ;; *) pass "$1 (web server saw: ${got:-nothing})" ;; esac
}

mark; check "baseline: client reaches the web server directly" fetch
expect_seen "baseline: through the gateway's WAN address" 192.0.2.2

# --- Up, as cpxy-router.service does ---
check "gateway.sh up" in_gw "$GW" up
[ "$(in_gw cat /sys/class/net/cpxy0/mtu)" = 1280 ] && pass "cpxy0 MTU is 1280" || fail "cpxy0 MTU is not 1280"
# ip netns exec execs, so $! is gateway.sh itself
ip netns exec gw "$GW" run >"$WORK/engine.log" 2>&1 &
RUN_PID=$!
PIDS="$PIDS $RUN_PID"
sleep 2

mark; check "client reaches the web server through the proxy" fetch
expect_seen "request came through the cpxy server" 93.184.216.34
check "a 2 MB download completes at MTU 1280" fetch_big
check "local-region TCP is reachable" in_client curl -sS --max-time 5 --noproxy '*' http://114.114.114.114:8000/index.html
grep -q '^192.0.2.2 ' "$WORK/direct.log" && pass "local-region TCP goes direct" || fail "local-region TCP was not direct"
mark; check "the gateway's own traffic goes out directly" in_gw curl -sS --max-time 5 --noproxy '*' "$URL/index.html"
expect_seen "gateway traffic is not captured" 192.0.2.2
check "up again (a restart) is idempotent" in_gw "$GW" up
mark; check "still proxied after up again" fetch
expect_seen "still through the cpxy server" 93.184.216.34

# --- Fail closed: the engine dies; run restarts it 2s later, routing stays meanwhile ---
in_gw pkill -9 -x cpxy-router
sleep 0.3
mark; TIMEOUT=1 check_not "engine down: client traffic is refused" fetch
expect_not_direct "engine down: nothing leaked via the WAN"
sleep 3
mark; check "engine restarted by gateway.sh run" fetch
expect_seen "restarted engine proxies again" 93.184.216.34

# --- Stop, as systemd does: TERM to run, then down ---
kill -TERM "$RUN_PID"
if wait "$RUN_PID"; then pass "run stops cleanly on TERM"; else fail "run returned an error on TERM"; fi
sleep 0.5
check_not "run stopped the engine too" in_gw pgrep -x cpxy-router
check_not "engine never panicked" grep -m3 panicked "$WORK/engine.log"
check "gateway.sh down" in_gw "$GW" down
[ "$(in_gw ip -4 rule show | grep -c 910)" = 0 ] && pass "down: rules removed" || fail "down: rules remain"
in_gw ip link show cpxy0 >/dev/null 2>&1 && fail "down: cpxy0 remains" || pass "down: cpxy0 removed"
mark; check "down: client reaches the internet directly again" fetch
expect_seen "down: direct again via the WAN" 192.0.2.2

# --- Config errors are refused before touching routing ---
printf 'INTERFACES=tailscale0\n' >"$CPXY_CONF"
check_not "missing SERVER is refused" in_gw "$GW" up
[ "$(in_gw ip -4 rule show | grep -c 910)" = 0 ] && pass "refused config installs no routing" || fail "refused config installed routing"

if [ "$FAILED" = 0 ]; then
	echo "lab passed"
else
	echo "lab FAILED"
	for f in engine server; do echo "== $f.log"; tail -n 15 "$WORK/$f.log" 2>/dev/null; done
fi
exit "$FAILED"
