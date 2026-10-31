#!/bin/sh
# Routing lab: checks the policy routing and fail-closed behaviour with the real binaries, in
# throwaway network namespaces (no root needed; uses an unprivileged user namespace).
#
#   lan (192.168.8.50) -- router (LAN 192.168.8.1, WAN 192.0.2.2, masquerading) -- inet
#   inet: a cpxy server on 192.0.2.1:8443 and a web server on 93.184.216.34:8000
#
# IPv6 likewise, without NAT: lan 2001:db8:8::50, router 2001:db8:8::1 and 2001:db8:2::2, and the
# web servers also on 2001:db8:100::34 (not local-region) and 2400:3200::114 (local-region).
#
# The router mimics OpenWrt: strict rp_filter, forward policy drop, and the package's real
# routing code (net.sh), native packet engine and nftables snippet. It does not run fw4 or dnsmasq.
#
# usage: lab.sh <dir with cpxy-router, cpxy-client and cpxy-server>
set -u

if [ -z "${CPXY_LAB_INNER:-}" ]; then
	exec env CPXY_LAB_INNER=1 unshare -Urmn --propagation private sh "$0" "$@"
fi

BIN="$(cd "$1" && pwd)"
NET="$(cd "$(dirname "$0")/../../common" && pwd)/net.sh"
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

IPV6_ON='sysctl -qw net.ipv6.conf.all.disable_ipv6=0 net.ipv6.conf.default.disable_ipv6=0'
in_inet sh -c "$IPV6_ON"'; ip link set lo up; ip addr add 93.184.216.34/32 dev lo; ip addr add 192.0.2.1/24 dev wan0
	ip -6 addr add 2001:db8:100::34/128 dev lo; ip -6 addr add 2400:3200::114/128 dev lo
	ip -6 addr add 2001:db8:2::1/64 dev wan0 nodad; ip link set wan0 up
	ip route add 192.168.8.0/24 via 192.0.2.2; ip -6 route add 2001:db8:8::/64 via 2001:db8:2::2'
in_router sh -c "$IPV6_ON"'; ip link set lo up; ip addr add 192.0.2.2/24 dev wan0r; ip addr add 192.168.8.1/24 dev lan0r
	ip -6 addr add 2001:db8:2::2/64 dev wan0r nodad; ip -6 addr add 2001:db8:8::1/64 dev lan0r nodad
	ip link set wan0r up; ip link set lan0r up; ip route add default via 192.0.2.1; ip -6 route add default via 2001:db8:2::1
	sysctl -qw net.ipv4.ip_forward=1 net.ipv4.conf.all.rp_filter=1 net.ipv4.conf.default.rp_filter=1 net.ipv6.conf.all.forwarding=1'
in_lan sh -c "$IPV6_ON"'; ip link set lo up; ip addr add 192.168.8.50/24 dev lan0; ip -6 addr add 2001:db8:8::50/64 dev lan0 nodad
	ip link set lan0 up; ip route add default via 192.168.8.1; ip -6 route add default via 2001:db8:8::1'

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
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 93.184.216.34 2>>"$WORK/web.log")
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 2001:db8:100::34 2>>"$WORK/web.log")
in_inet ip addr add 114.114.114.114/32 dev lo
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 114.114.114.114 2>>"$WORK/direct.log")
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 2400:3200::114 2>>"$WORK/direct.log")
# UDP echo on 443 and 9999: replies with the source address it saw
bg in_inet python3 -c '
import selectors, socket
sel = selectors.DefaultSelector()
for ip in ("93.184.216.34", "2001:db8:100::34"):
  for port in (443, 9999):
    s = socket.socket(socket.AF_INET6 if ":" in ip else socket.AF_INET, socket.SOCK_DGRAM); s.bind((ip, port)); sel.register(s, selectors.EVENT_READ)
while True:
    for key, _ in sel.select():
        _, addr = key.fileobj.recvfrom(64); key.fileobj.sendto(addr[0].encode(), addr)
'
sleep 1

URL=http://93.184.216.34:8000/index.html
URL_V6='http://[2001:db8:100::34]:8000/index.html'
fetch() { in_lan curl -gsS --max-time 5 --noproxy '*' "${1:-$URL}"; }
# Who did the web server see since mark? (192.0.2.2 is the router's WAN address: traffic that went direct)
mark() { MARK="$(wc -l <"$WORK/web.log")"; }
seen() { sleep 0.3; tail -n +"$((MARK + 1))" "$WORK/web.log" | grep -o '^[0-9a-f.:]*' | sort -u | tr '\n' ' ' | sed 's/ $//'; }
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
mark; check "baseline: LAN reaches the web server directly over IPv6" fetch "$URL_V6"
expect_seen "baseline: IPv6 request arrives from the LAN address" 2001:db8:8::50

# The production router uses both family exports and an independent SOCKS5 process.
"$BIN/cpxy-router" --export-direct-routes >"$WORK/direct.cidrs"
"$BIN/cpxy-router" --export-direct-routes-v6 >"$WORK/direct6.cidrs"
GEO="$(dirname "$NET")/geo.sh"
kernel_up() {
    in_router sh -c ". '$NET'; . '$GEO'; cpxy_net_select '$1' '$2' '$3'; CPXY_GEO6_ROUTES='$WORK/direct6.cidrs'; CPXY_GEO6_GATEWAY=2001:db8:2::1; CPXY_GEO6_WAN=wan0r; cpxy_geo_up '$WORK/direct.cidrs' 192.0.2.1 wan0r '$4'"
}
bg ip netns exec router env CONFIG=http://:lab@192.0.2.1:8443 SOCKS5_PROXY_LISTEN=127.0.0.1:1080 "$BIN/cpxy-client" >"$WORK/socks.log" 2>&1
SOCKS_PID=$!
# --- Start the service pieces exactly as the init script does ---
kernel_up cpxy0 100 9100 lan0r
check "policy routing installs" in_router ip rule show
# ip netns exec execs, so $! is the native worker itself.
bg ip netns exec router "$BIN/cpxy-router" --socks5-proxy 127.0.0.1:1080 --tun cpxy0 --api-listen 127.0.0.1:3010 >"$WORK/tun.log" 2>&1
TUN_PID=$!
sleep 2

check "offline traffic page is served" in_router curl -fsS --max-time 3 --noproxy '*' http://127.0.0.1:3010/
check_not "occupied API port prevents startup" in_router "$BIN/cpxy-router" --socks5-proxy 127.0.0.1:1080 --tun api-conflict --api-listen 127.0.0.1:3010
check_not "API bind failure creates no TUN device" in_router ip link show api-conflict
in_router curl -fsS --max-time 3 --noproxy '*' http://127.0.0.1:3010/api/traffic >"$WORK/api-start.json"

echo "--- running"
if [ -n "${CPXY_LAB_DEBUG:-}" ]; then
	in_router sh -c 'ip rule show; ip route show table 100; ip addr show cpxy0; ip route get 93.184.216.34 from 192.168.8.50 iif lan0r'
fi
mark; check "LAN reaches the web server through the proxy" fetch
got="$(seen)"
if [ -n "$got" ] && [ "$got" != 192.0.2.2 ]; then pass "request came through cpxy, not the router WAN (web server saw: $got)"
else fail "request did not go through the proxy (web server saw: '${got}')"; fi
# The successful proxied TCP appears in the same engine history exposed to Android.
sleep 1
check "traffic API reports real engine connections and byte totals" in_router python3 -c '
import json, urllib.request
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path):
    return json.load(opener.open("http://127.0.0.1:3010/api/" + path, timeout=3))
traffic, page = get("traffic"), get("connections")
assert traffic["running"] and traffic["sent"] > 0 and traffic["received"] > 0, traffic
assert traffic["session"] == page["session"]
assert any(r["event"]["ip"] == "93.184.216.34" and r["event"]["outbound"] == "proxy" and r["event"]["error"] is None for r in page["records"]), page
'
# A reload runs cpxy_net_up again while native worker keeps running; it must stay attached to cpxy0
kernel_up cpxy0 100 9100 lan0r
mark; check "reload: LAN still reaches the web server through the proxy" fetch
got="$(seen)"
if [ -n "$got" ] && [ "$got" != 192.0.2.2 ]; then pass "reload: still through cpxy"
else fail "reload: request did not go through the proxy (web server saw: '${got}')"; fi
mark; check "router's own traffic still goes out WAN" in_router curl -sS --max-time 5 --noproxy '*' "$URL"
expect_seen "router traffic is not captured" 192.0.2.2
check "LAN can still reach the router itself" in_lan ping -c1 -W2 192.168.8.1
# udp_probe <port> [host]: prints what a UDP datagram from the LAN to the internet echo server got back
udp_probe() {
	in_lan sh -c "timeout 3 python3 - $1 ${2:-93.184.216.34} <<'PY'
import errno,socket,sys
s=socket.socket(socket.AF_INET6 if ':' in sys.argv[2] else socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2)
try:
 s.connect((sys.argv[2],int(sys.argv[1]))); s.send(b'x')
 print('reply from', s.recv(64).decode())
except socket.timeout: print('timeout')
except OSError as e:
 if e.errno in (errno.ECONNREFUSED,errno.EACCES,errno.EHOSTUNREACH,errno.ENETUNREACH): print('refused')
 else: raise
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

# IPv6 takes the same paths, without NAT
mark; check "LAN reaches the web server through the proxy over IPv6" fetch "$URL_V6"
got="$(seen)"
case "$got" in
'' | *2001:db8:2::2* | *2001:db8:8::50*) fail "IPv6 request did not go through the proxy (web server saw: '${got}')" ;;
*) pass "IPv6 request came through cpxy (web server saw: $got)" ;;
esac
check "local-region TCP over IPv6 is reachable" in_lan curl -gsS --max-time 5 --noproxy '*' 'http://[2400:3200::114]:8000/index.html'
grep -q '^2001:db8:8::50 ' "$WORK/direct.log" && pass "local-region TCP over IPv6 is forwarded natively" ||
	fail "local-region TCP over IPv6 was not direct"
got="$(udp_probe 443 2001:db8:100::34)"
[ "$got" = refused ] && pass "QUIC over IPv6 gets an immediate ICMPv6 refusal" || fail "IPv6 UDP 443 was not refused (got: $got)"
got="$(udp_probe 9999 2001:db8:100::34)"
[ "$got" = "reply from 2001:db8:8::50" ] && pass "other UDP over IPv6 is forwarded directly ($got)" ||
	fail "IPv6 UDP 9999 was not forwarded directly (got: $got)"

# --- A second network selects a genuinely different upstream server. ---
bg ip netns exec upstream "$BIN/cpxy-server" --key guestlab 192.0.3.1:8444 >"$WORK/guest-server.log" 2>&1
kernel_up cpxy1 101 9110 guest0r
bg ip netns exec router env CONFIG=http://:guestlab@192.0.3.1:8444 SOCKS5_PROXY_LISTEN=127.0.0.1:1081 "$BIN/cpxy-client" >"$WORK/guest-socks.log" 2>&1
bg ip netns exec router "$BIN/cpxy-router" --socks5-proxy 127.0.0.1:1081 --tun cpxy1 >"$WORK/guest-tun.log" 2>&1
GUEST_PID=$!
sleep 1
guest_fetch() { in_guest curl -sS --max-time 5 --noproxy '*' "$URL"; }
mark; check "guest uses second upstream" guest_fetch
expect_seen "guest upstream has its own source address" 192.0.3.1
mark; check "main still uses first upstream" fetch
expect_seen "main upstream unchanged" 93.184.216.34
kernel_up cpxy1 101 9110 guest0r
mark; check "guest reload preserves second upstream" guest_fetch
expect_seen "guest reload keeps second source address" 192.0.3.1
kill "$GUEST_PID"
sleep 0.5
mark; check_not "guest worker failure is closed" guest_fetch
expect_not_direct "guest failure has no WAN fallback"
mark; check "guest failure leaves main working" fetch
expect_seen "main upstream after guest failure" 93.184.216.34
in_router sh -c ". '$NET'; . '$GEO'; cpxy_net_select cpxy1 101 9110; cpxy_geo_down"
mark; check "guest stop restores direct routing" guest_fetch
expect_seen "guest direct after stop" 192.0.2.2
mark; check "guest stop leaves main working" fetch
expect_seen "main upstream after guest stop" 93.184.216.34

# --- Fail closed: native worker dies ---
kill "$TUN_PID"; sleep 1
mark; check_not "native worker down: LAN traffic is refused" fetch
expect_not_direct "native worker down: nothing leaked via the WAN"

# The persistent TUN survives worker exit and can be attached again without redoing routes.
check_not "worker exit closes the traffic listener" in_router curl -fsS --max-time 2 --noproxy '*' http://127.0.0.1:3010/api/traffic
check "worker exit preserves the TUN device" in_router ip link show cpxy0
bg ip netns exec router "$BIN/cpxy-router" --socks5-proxy 127.0.0.1:1080 --tun cpxy0 --api-listen 127.0.0.1:3010 >>"$WORK/tun.log" 2>&1
TUN_PID=$!
sleep 1
check "worker restart changes traffic API session" in_router python3 -c '
import json, sys, urllib.request
old = json.load(open(sys.argv[1]))
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
new = json.load(opener.open("http://127.0.0.1:3010/api/traffic", timeout=3))
assert old["session"] != new["session"]
' "$WORK/api-start.json"
mark; check "worker restart restores proxied TCP" fetch
expect_not_direct "worker restart still uses cpxy"

kill "$SOCKS_PID"; sleep 0.5
mark; check_not "SOCKS5 down: overseas TCP fails closed" fetch
expect_not_direct "SOCKS5 down: no direct fallback"
check "SOCKS5 down: regional IPv6 stays native" in_lan curl -gfsS --max-time 3 --noproxy '*' 'http://[2400:3200::114]:8000/index.html'
bg ip netns exec router env CONFIG=http://:lab@192.0.2.1:8443 SOCKS5_PROXY_LISTEN=127.0.0.1:1080 "$BIN/cpxy-client" >>"$WORK/socks.log" 2>&1
SOCKS_PID=$!
sleep 0.5
kill "$SERVER_PID"; sleep 0.5
mark; check_not "server down: non-local TCP fails closed" fetch
expect_not_direct "server down: no direct fallback"
check "server down: local-region TCP still works" in_lan curl -sS --max-time 5 --noproxy '*' http://114.114.114.114:8000/index.html
kill -TERM "$TUN_PID"
if wait "$TUN_PID"; then pass "worker stops cleanly"; else fail "worker stop returned an error"; fi
check_not "worker never panicked" grep -m3 panicked "$WORK/tun.log"

# A real external SOCKS5 fixture must work after the cpxy server has stopped.
SOCKS_FIXTURE="$(cd "$(dirname "$0")" && pwd)/socks5-fixture.py"
DNS_FIXTURE="$(cd "$(dirname "$0")/../../../mobile-engine/test" && pwd)/dns.py"
bg ip netns exec inet python3 "$SOCKS_FIXTURE" 2001:db8:2::1 1082 >"$WORK/external-socks.log" 2>&1
in_inet ip addr add 8.8.8.8/32 dev lo
bg ip netns exec inet python3 "$DNS_FIXTURE" serve 114.114.114.114 0 cn.test=114.114.114.114 overseas.test=93.184.216.34 >"$WORK/cn-dns.log" 2>&1
bg ip netns exec inet python3 "$DNS_FIXTURE" serve 8.8.8.8 0 overseas.test=93.184.216.34 overseas.test=2001:db8:100::34 >"$WORK/overseas-dns.log" 2>&1
bg ip netns exec router "$BIN/cpxy-router" --socks5-proxy '[2001:db8:2::1]:1082' --tun cpxy0 --dns-listen '[::]:5335' --dns-upstream 114.114.114.114 --dns-alternative 8.8.8.8 >"$WORK/external-router.log" 2>&1
TUN_PID=$!
sleep 1
mark; check "external SOCKS5 without cpxy server relays IPv4" fetch
expect_seen "external SOCKS5 uses proxy exit" 93.184.216.34
check "external SOCKS5 relays IPv6" fetch "$URL_V6"
check "DNS redirect installs for both address families" in_router nft -f - <<'NFT'
table inet cpxy_test_dns {
    chain prerouting {
        type nat hook prerouting priority dstnat;
        iifname "lan0r" udp dport 53 redirect to :5335
        iifname "lan0r" tcp dport 53 redirect to :5335
    }
}
NFT
dns_query() { in_lan python3 "$DNS_FIXTURE" query "$@"; }
[ "$(dns_query 8.8.4.4 cn.test)" = 'NOERROR 114.114.114.114' ] && pass "Chinese DNS answer via direct UDP" || fail "Chinese DNS answer"
[ "$(dns_query 8.8.4.4 overseas.test tcp)" = 'NOERROR 93.184.216.34' ] && pass "IPv4 TCP DNS redirected and proxied" || fail "IPv4 TCP DNS"
[ "$(dns_query 2001:4860:4860::8844 overseas.test udp AAAA)" = 'NOERROR 2001:db8:100::34' ] && pass "IPv6 UDP DNS redirected, AAAA retained" || fail "IPv6 UDP DNS"
[ "$(dns_query 2001:4860:4860::8844 overseas.test tcp)" = 'NOERROR 93.184.216.34' ] && pass "IPv6 TCP DNS redirected" || fail "IPv6 TCP DNS"
check "overseas UDP DNS specification used SOCKS5 TCP" grep -q 'CONNECT 8.8.8.8 53' "$WORK/external-socks.log"
check_not "Chinese DNS never enters SOCKS5" grep -q 'CONNECT 114.114.114.114 53' "$WORK/external-socks.log"
kill "$TUN_PID"; wait "$TUN_PID"
in_router nft delete table inet cpxy_test_dns

# --- Stop: the original setup is back ---
in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_down"
[ "$(in_router ip -4 rule show | grep -c 910)" = 0 ] && pass "stop: rules removed" || fail "stop: rules remain"
[ "$(in_router ip -4 route show table 100 | wc -l)" = 0 ] && pass "stop: table 100 empty" || fail "stop: table 100 not empty"
[ "$(in_router ip -6 rule show | grep -c 910)" = 0 ] && pass "stop: IPv6 rules removed" || fail "stop: IPv6 rules remain"
[ "$(in_router ip -6 route show table 100 | wc -l)" = 0 ] && pass "stop: IPv6 table 100 empty" || fail "stop: IPv6 table 100 not empty"
mark; check "stop: LAN reaches the internet directly again" fetch
expect_seen "stop: direct again via the WAN" 192.0.2.2

. "$(dirname "$0")/geo-lab.sh"

if [ "$FAILED" = 0 ]; then
	echo "lab passed"
else
	echo "lab FAILED"
	for f in tun server external-router external-socks cn-dns overseas-dns; do echo "== $f.log"; tail -n 15 "$WORK/$f.log" 2>/dev/null; done
fi
exit "$FAILED"
