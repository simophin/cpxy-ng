#!/bin/sh
# Routing lab: checks the policy routing and fail-closed behaviour with the real binaries, in
# throwaway network namespaces (no root needed; uses an unprivileged user namespace).
#
#   lan (192.168.8.50) -- router (LAN 192.168.8.1, WAN 192.0.2.2, masquerading) -- inet
#   inet: a cpxy server on 192.0.2.1:8443 and a web server on 93.184.216.34:8000
#
# The router mimics OpenWrt: strict rp_filter, forward policy drop, and the package's real
# routing code (net.sh), native packet engine and nftables snippet. It does not run fw4 or dnsmasq.
#
# usage: lab.sh <dir with cpxy-router and cpxy-server>
set -u

if [ -z "${CPXY_LAB_INNER:-}" ]; then
	exec env CPXY_LAB_INNER=1 unshare -Urmn --propagation private sh "$0" "$@"
fi

BIN="$(cd "$1" && pwd)"
PKG="$(cd "$(dirname "$0")/../files" && pwd)"
WORK="$(mktemp -d)"
FAILED=0
PIDS=""
# $PIDS can be wrapper subshells, so also kill whatever still runs inside the lab's namespaces
cleanup() {
	for p in $PIDS $(for ns in inet router lan guest upstream; do ip netns pids "$ns" 2>/dev/null; done); do kill "$p" 2>/dev/null; done
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

mount -t tmpfs tmpfs /run && mkdir -p /run/netns
for ns in inet router lan guest upstream; do ip netns add "$ns"; done
ip link add wan0 type veth peer name wan0r
ip link set wan0 netns inet; ip link set wan0r netns router
ip link add lan0r type veth peer name lan0
ip link set lan0r netns router; ip link set lan0 netns lan
ip link add guest0r type veth peer name guest0
ip link set guest0r netns router; ip link set guest0 netns guest
ip link add up0 type veth peer name up0i
ip link set up0 netns upstream; ip link set up0i netns inet

in_inet() { ip netns exec inet "$@"; }
in_router() { ip netns exec router "$@"; }
in_lan() { ip netns exec lan "$@"; }
in_guest() { ip netns exec guest "$@"; }

in_inet sh -c 'ip link set lo up; ip addr add 93.184.216.34/32 dev lo; ip addr add 192.0.2.1/24 dev wan0; ip link set wan0 up
	ip route add 192.168.8.0/24 via 192.0.2.2'
in_router sh -c 'ip link set lo up; ip addr add 192.0.2.2/24 dev wan0r; ip addr add 192.168.8.1/24 dev lan0r
	ip link set wan0r up; ip link set lan0r up; ip route add default via 192.0.2.1
	sysctl -qw net.ipv4.ip_forward=1 net.ipv4.conf.all.rp_filter=1 net.ipv4.conf.default.rp_filter=1'
in_lan sh -c 'ip link set lo up; ip addr add 192.168.8.50/24 dev lan0; ip link set lan0 up; ip route add default via 192.168.8.1'

in_inet sh -c 'ip addr add 192.0.3.2/24 dev up0i; ip link set up0i up; sysctl -qw net.ipv4.ip_forward=1'
ip netns exec upstream sh -c 'ip link set lo up; ip addr add 192.0.3.1/24 dev up0; ip link set up0 up; ip route add default via 192.0.3.2'
in_router sh -c 'ip addr add 192.168.9.1/24 dev guest0r; ip link set guest0r up; ip route add 192.0.3.0/24 via 192.0.2.1'
in_guest sh -c 'ip link set lo up; ip addr add 192.168.9.50/24 dev guest0; ip link set guest0 up; ip route add default via 192.168.9.1'

# Router firewall, shaped like fw4: forward drops by default, the package snippet comes first,
# LAN may go to WAN (the original setup) and to the TUN device (the cpxy zone).
cat >"$WORK/fw.nft" <<EOF
table inet fw4 {
	chain forward {
		type filter hook forward priority filter; policy drop;
		oifname { "cpxy0", "cpxy1" } meta l4proto udp reject
		ct state established,related accept
		iifname "lan0r" oifname { "wan0r", "cpxy0" } accept
		iifname "guest0r" oifname { "wan0r", "cpxy1" } accept
	}
}
table ip nat {
	chain postrouting {
		type nat hook postrouting priority srcnat; policy accept;
		oifname "wan0r" masquerade
	}
}
EOF
check "router firewall loads" in_router nft -f "$WORK/fw.nft"

# --- Internet side ---
mkdir "$WORK/www" && echo hello >"$WORK/www/index.html"
bg ip netns exec inet "$BIN/cpxy-server" --key lab 192.0.2.1:8443 >"$WORK/server.log" 2>&1
SERVER_PID=$!
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 93.184.216.34 2>"$WORK/web.log")
in_inet ip addr add 114.114.114.114/32 dev lo
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 114.114.114.114 2>"$WORK/direct.log")
# UDP echo on 443 and 9999: replies with the source address it saw
bg in_inet python3 -c '
import selectors, socket
sel = selectors.DefaultSelector()
for port in (443, 9999):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(("93.184.216.34", port)); sel.register(s, selectors.EVENT_READ)
while True:
    for key, _ in sel.select():
        _, addr = key.fileobj.recvfrom(64); key.fileobj.sendto(addr[0].encode(), addr)
'
sleep 1

URL=http://93.184.216.34:8000/index.html
fetch() { in_lan curl -sS --max-time 5 --noproxy '*' "$URL"; }
# Who did the web server see since mark? (192.0.2.2 is the router's WAN address: traffic that went direct)
mark() { MARK="$(wc -l <"$WORK/web.log")"; }
seen() { sleep 0.3; tail -n +"$((MARK + 1))" "$WORK/web.log" | grep -o '^[0-9.]*' | sort -u | tr '\n' ' ' | sed 's/ $//'; }
expect_seen() { # expect_seen <description> <sources>
	got="$(seen)"
	if [ "$got" = "$2" ]; then pass "$1 (web server saw: ${got:-nothing})"; else fail "$1 (web server saw: '${got}', wanted '$2')"; fi
}
expect_not_direct() {
	got="$(seen)"
	case "$got" in *192.0.2.2*) fail "$1 (traffic left via the WAN)" ;; *) pass "$1 (web server saw: ${got:-nothing})" ;; esac
}

# --- Before the service: the LAN reaches the internet directly, through WAN NAT ---
mark; check "baseline: LAN reaches the web server directly" fetch
expect_seen "baseline: request arrives from the router's WAN address" 192.0.2.2

# --- Start the service pieces exactly as the init script does ---
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_up lan0r"
check "policy routing installs" in_router ip rule show
# ip netns exec execs, so $! is the native worker itself.
bg ip netns exec router env SERVER=http://:lab@192.0.2.1:8443 "$BIN/cpxy-router" --tun cpxy0 >"$WORK/tun.log" 2>&1
TUN_PID=$!
sleep 2

echo "--- running"
if [ -n "${CPXY_LAB_DEBUG:-}" ]; then
	in_router sh -c 'ip rule show; ip route show table 100; ip addr show cpxy0; ip route get 93.184.216.34 from 192.168.8.50 iif lan0r'
fi
mark; check "LAN reaches the web server through the proxy" fetch
got="$(seen)"
if [ -n "$got" ] && [ "$got" != 192.0.2.2 ]; then pass "request came through cpxy, not the router WAN (web server saw: $got)"
else fail "request did not go through the proxy (web server saw: '${got}')"; fi
# A reload runs cpxy_net_up again while native worker keeps running; it must stay attached to cpxy0
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_up lan0r"
mark; check "reload: LAN still reaches the web server through the proxy" fetch
got="$(seen)"
if [ -n "$got" ] && [ "$got" != 192.0.2.2 ]; then pass "reload: still through cpxy"
else fail "reload: request did not go through the proxy (web server saw: '${got}')"; fi
mark; check "router's own traffic still goes out WAN" in_router curl -sS --max-time 5 --noproxy '*' "$URL"
expect_seen "router traffic is not captured" 192.0.2.2
check "LAN can still reach the router itself" in_lan ping -c1 -W2 192.168.8.1
# udp_probe <port>: prints what a UDP datagram from the LAN to the internet echo server got back
udp_probe() {
	in_lan sh -c "timeout 3 python3 - $1 <<'PY'
import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2); s.connect(('93.184.216.34',int(sys.argv[1]))); s.send(b'x')
try: print('reply from', s.recv(64).decode())
except ConnectionRefusedError: print('refused')
except socket.timeout: print('timeout')
PY"
}
# The echo server answers on 443 too, so a refusal there can only come from the router
got="$(udp_probe 443)"
[ "$got" = refused ] && pass "QUIC (UDP 443) gets an immediate ICMP refusal" || fail "UDP 443 was not refused (got: $got)"
got="$(udp_probe 9999)"
[ "$got" = "reply from 192.0.2.2" ] && pass "other UDP goes out the WAN directly ($got)" ||
	fail "UDP 9999 did not go out the WAN directly (got: $got)"

# The shared regional policy opens direct TCP sockets from the router.
check "local-region TCP is reachable" in_lan curl -sS --max-time 5 --noproxy '*' http://114.114.114.114:8000/index.html
grep -q '^192.0.2.2 ' "$WORK/direct.log" && pass "local-region TCP goes direct" || fail "local-region TCP was not direct"

# --- A second network selects a genuinely different upstream server. ---
bg ip netns exec upstream "$BIN/cpxy-server" --key guestlab 192.0.3.1:8444 >"$WORK/guest-server.log" 2>&1
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_select cpxy1 101 9110; cpxy_net_up guest0r"
bg ip netns exec router env SERVER=http://:guestlab@192.0.3.1:8444 "$BIN/cpxy-router" --tun cpxy1 >"$WORK/guest-tun.log" 2>&1
GUEST_PID=$!
sleep 1
guest_fetch() { in_guest curl -sS --max-time 5 --noproxy '*' "$URL"; }
mark; check "guest uses second upstream" guest_fetch
expect_seen "guest upstream has its own source address" 192.0.3.1
mark; check "main still uses first upstream" fetch
expect_seen "main upstream unchanged" 93.184.216.34
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_select cpxy1 101 9110; cpxy_net_up guest0r"
mark; check "guest reload preserves second upstream" guest_fetch
expect_seen "guest reload keeps second source address" 192.0.3.1
kill "$GUEST_PID"
sleep 0.5
mark; check_not "guest worker failure is closed" guest_fetch
expect_not_direct "guest failure has no WAN fallback"
mark; check "guest failure leaves main working" fetch
expect_seen "main upstream after guest failure" 93.184.216.34
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_select cpxy1 101 9110; cpxy_net_down_routing"
mark; check "guest stop restores direct routing" guest_fetch
expect_seen "guest direct after stop" 192.0.2.2
mark; check "guest stop leaves main working" fetch
expect_seen "main upstream after guest stop" 93.184.216.34

# --- Fail closed: native worker dies ---
kill "$TUN_PID"; sleep 1
mark; check_not "native worker down: LAN traffic is refused" fetch
expect_not_direct "native worker down: nothing leaked via the WAN"

# The persistent TUN survives worker exit and can be attached again without redoing routes.
check "worker exit preserves the TUN device" in_router ip link show cpxy0
bg ip netns exec router env SERVER=http://:lab@192.0.2.1:8443 "$BIN/cpxy-router" --tun cpxy0 >>"$WORK/tun.log" 2>&1
TUN_PID=$!
sleep 1
mark; check "worker restart restores proxied TCP" fetch
expect_not_direct "worker restart still uses cpxy"

kill "$SERVER_PID"; sleep 0.5
mark; check_not "server down: non-local TCP fails closed" fetch
expect_not_direct "server down: no direct fallback"
check "server down: local-region TCP still works" in_lan curl -sS --max-time 5 --noproxy '*' http://114.114.114.114:8000/index.html
kill -TERM "$TUN_PID"
if wait "$TUN_PID"; then pass "worker stops cleanly"; else fail "worker stop returned an error"; fi
check_not "worker never panicked" grep -m3 panicked "$WORK/tun.log"

# --- Stop: the original setup is back ---
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_down_routing"
[ "$(in_router ip -4 rule show | grep -c 910)" = 0 ] && pass "stop: rules removed" || fail "stop: rules remain"
[ "$(in_router ip -4 route show table 100 | wc -l)" = 0 ] && pass "stop: table 100 empty" || fail "stop: table 100 not empty"
mark; check "stop: LAN reaches the internet directly again" fetch
expect_seen "stop: direct again via the WAN" 192.0.2.2

if [ "$FAILED" = 0 ]; then
	echo "lab passed"
else
	echo "lab FAILED"
	for f in tun server; do echo "== $f.log"; tail -n 15 "$WORK/$f.log" 2>/dev/null; done
fi
exit "$FAILED"
