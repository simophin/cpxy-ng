#!/bin/sh
# Routing lab: checks the nftables policy and fail-closed behaviour with the real binaries, in
# throwaway network namespaces (no root needed; uses an unprivileged user namespace).
#
#   lan (192.168.8.50) -- router (LAN 192.168.8.1, WAN 192.0.2.2, masquerading) -- inet
#   guest (192.168.9.50) -/
#   inet: a cpxy server on [2001:db8:2::1]:8443, web servers on 93.184.216.34:8000 (overseas)
#   and 114.114.114.114:8000 (CN), and a second cpxy server in another namespace, 192.0.3.1:8444
#
# IPv6 likewise, without NAT: lan 2001:db8:8::50, router 2001:db8:8::1 and 2001:db8:2::2, and the
# web servers also on 2001:db8:100::34 (not local-region) and 2400:3200::114 (local-region).
#
# The router mimics OpenWrt: strict rp_filter, forward policy drop, LAN-to-WAN forwarding, a
# DNS redirect like fw4's, and the package's real nftables policy (nft.sh), cpxy-client
# transparent listeners and cpxy-router's `cpxy dns` workers. It does not run fw4 or dnsmasq.
#
# usage: lab.sh <dir with cpxy>
set -u

if [ -z "${CPXY_LAB_INNER:-}" ]; then
	exec env CPXY_LAB_INNER=1 unshare -Urmn --propagation private sh "$0" "$@"
fi

BIN="$(cd "$1" && pwd)"
# Plain logs, as the init scripts run the binaries
export NO_COLOR=1
HERE="$(cd "$(dirname "$0")" && pwd)"
NFT="$HERE/../files/usr/libexec/cpxy/nft.sh"
DNS_FIXTURE="$HERE/../../../mobile-engine/test/dns.py"
WORK="$(mktemp -d)"
FAILED=0
PIDS=""
# $PIDS can be wrapper subshells, so also kill whatever still runs inside the lab's namespaces
cleanup() {
	for p in $PIDS $(for ns in inet router lan guest upstream tail; do ip netns pids "$ns" 2>/dev/null; done); do kill "$p" 2>/dev/null; done
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
for ns in inet router lan guest upstream tail; do ip netns add "$ns"; done
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
	ip addr add 114.114.114.114/32 dev lo; ip addr add 8.8.8.8/32 dev lo
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
# A tailnet-like path, as Tailscale sets up: a peer at a non-private address reached through its
# own device by a policy-routing table. Its TCP must go there directly, not through the proxy.
ip link add tail0r type veth peer name tail0
ip link set tail0r netns router; ip link set tail0 netns tail
in_router sh -c 'ip addr add 100.64.0.1/24 dev tail0r; ip link set tail0r up
	ip rule add priority 5270 lookup 52; ip route add 100.100.1.1/32 via 100.64.0.2 dev tail0r table 52'
ip netns exec tail sh -c 'ip link set lo up; ip addr add 100.100.1.1/32 dev lo; ip addr add 100.64.0.2/24 dev tail0
	ip link set tail0 up; ip route add default via 100.64.0.1'

# Router firewall, shaped like fw4: forward drops by default, the LANs may go to the WAN, and
# their DNS is redirected to their instance's split resolver (fw4's DNAT runs at dstnat).
cat >"$WORK/fw.nft" <<EOF
table inet fw4 {
	chain forward {
		type filter hook forward priority filter; policy drop;
		ct state established,related accept
		iifname { "lan0r", "guest0r" } oifname { "wan0r", "tail0r" } accept
	}
	chain dstnat {
		type nat hook prerouting priority dstnat; policy accept;
		iifname "lan0r" meta l4proto { tcp, udp } th dport 53 redirect to :5335
		iifname "guest0r" meta l4proto { tcp, udp } th dport 53 redirect to :5345
	}
	chain srcnat {
		type nat hook postrouting priority srcnat; policy accept;
		oifname "wan0r" meta nfproto ipv4 masquerade
	}
}
EOF
check "router firewall loads" in_router nft -f "$WORK/fw.nft"

# --- Internet side ---
mkdir "$WORK/www" && echo hello >"$WORK/www/index.html"
bg ip netns exec inet "$BIN/cpxy" server --key lab --listen '[2001:db8:2::1]:8443' >"$WORK/server.log" 2>&1
SERVER_PID=$!
bg ip netns exec upstream "$BIN/cpxy" server --key guestlab --listen 192.0.3.1:8444 >"$WORK/guest-server.log" 2>&1
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 93.184.216.34 2>>"$WORK/web.log")
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 2001:db8:100::34 2>>"$WORK/web.log")
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 114.114.114.114 2>>"$WORK/direct.log")
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 2400:3200::114 2>>"$WORK/direct.log")
(cd "$WORK/www" && bg in_router python3 -m http.server 8080 --bind 192.168.8.1 2>>"$WORK/router-web.log")
(cd "$WORK/www" && bg ip netns exec tail python3 -m http.server 8000 --bind 100.100.1.1 2>>"$WORK/tail.log")
# UDP echo on 443 and 9999, and a TCP echo on 7000: each replies with the source address it saw
bg in_inet python3 -c '
import selectors, socket
sel = selectors.DefaultSelector()
for ip in ("93.184.216.34", "2001:db8:100::34"):
  for port in (443, 9999):
    s = socket.socket(socket.AF_INET6 if ":" in ip else socket.AF_INET, socket.SOCK_DGRAM); s.bind((ip, port)); sel.register(s, selectors.EVENT_READ)
t = socket.socket(); t.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); t.bind(("93.184.216.34", 7000)); t.listen(); t.setblocking(False)
sel.register(t, selectors.EVENT_READ)
while True:
    for key, _ in sel.select():
        s = key.fileobj
        if s is t:
            c, _ = t.accept(); sel.register(c, selectors.EVENT_READ)
        elif s.type == socket.SOCK_STREAM:
            data = s.recv(64)
            if data: s.sendall(data)
            else: sel.unregister(s); s.close()
        else:
            _, addr = s.recvfrom(64); s.sendto(addr[0].encode(), addr)
'
bg ip netns exec inet python3 "$DNS_FIXTURE" serve 114.114.114.114 0 cn.test=114.114.114.114 overseas.test=93.184.216.34 >"$WORK/cn-dns.log" 2>&1
bg ip netns exec inet python3 "$DNS_FIXTURE" serve 8.8.8.8 0 overseas.test=93.184.216.34 overseas.test=2001:db8:100::34 >"$WORK/overseas-dns.log" 2>&1
sleep 1

URL=http://93.184.216.34:8000/index.html
URL_V6='http://[2001:db8:100::34]:8000/index.html'
CN_URL=http://114.114.114.114:8000/index.html
fetch() { in_lan curl -gfsS --max-time 5 --noproxy '*' "${1:-$URL}"; }
guest_fetch() { in_guest curl -fsS --max-time 5 --noproxy '*' "${1:-$URL}"; }
# Who did the web servers see since mark? 192.0.2.2 is the router's WAN address: traffic that went
# direct. A cpxy server connects from the web server's own address (93.184.216.34 or
# 114.114.114.114) or, in the upstream namespace, from 192.0.3.1.
mark() { MARK="$(wc -l <"$WORK/web.log")"; DMARK="$(wc -l <"$WORK/direct.log")"; }
seen() { sleep 0.3; tail -n +"$((MARK + 1))" "$WORK/web.log" | grep -o '^[0-9a-f.:]*' | sort -u | tr '\n' ' ' | sed 's/ $//'; }
seen_direct() { sleep 0.3; tail -n +"$((DMARK + 1))" "$WORK/direct.log" | grep -o '^[0-9a-f.:]*' | sort -u | tr '\n' ' ' | sed 's/ $//'; }
expect_seen() { # expect_seen <description> <sources>
	got="$(seen)"
	if [ "$got" = "$2" ]; then pass "$1 (web server saw: ${got:-nothing})"; else fail "$1 (web server saw: '${got}', wanted '$2')"; fi
}
expect_seen_direct() {
	got="$(seen_direct)"
	if [ "$got" = "$2" ]; then pass "$1 (CN web server saw: ${got:-nothing})"; else fail "$1 (CN web server saw: '${got}', wanted '$2')"; fi
}
expect_not_direct() {
	got="$(seen)"
	case "$got" in *192.0.2.2*) fail "$1 (traffic left via the WAN)" ;; *) pass "$1 (web server saw: ${got:-nothing})" ;; esac
}
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
dns_query() { in_lan python3 "$DNS_FIXTURE" query "$@"; }

# --- Before the service: the LAN reaches the internet directly, through WAN NAT ---
mark; check "baseline: LAN reaches the web server directly" fetch
expect_seen "baseline: request arrives from the router's WAN address" 192.0.2.2

# --- The policy, built as cpxy-router's init script builds it ---
PLAN="$WORK/plan"
mkdir -p "$PLAN/routes"
# instance <name> <listener port> <dns port> <device> [proxy routes...]
instance() {
	inst="$1" port="$2" dns="$3" dev="$4"
	shift 4
	for route in "$@"; do set -- "$@" --proxy-route "$route"; shift; done
	{ "$BIN/cpxy" routes cn-direct "$@" && in_router sh -c ". '$NFT'; cpxy_nft_main_routes"; } >"$PLAN/routes/$inst" || return 1
	# shellcheck source=packaging/openwrt/files/usr/libexec/cpxy/nft.sh
	(. "$NFT"; cpxy_nft_state "$PLAN/$inst" "$port" "$dns" "$(sha256sum <"$PLAN/routes/$inst" | cut -d ' ' -f 1)" zone "$dev")
}
reinstall() { instance "$@" && apply; }
# wan0r is the uplink; the router's other devices are exempt, as the init script computes them.
apply() { in_router sh -c ". '$NFT'; cpxy_nft_render '$PLAN' \$(cpxy_nft_exempt wan0r) >'$WORK/ruleset' && cpxy_nft_apply '$WORK/ruleset'"; }
check "main instance policy renders" instance main 5100 5335 lan0r
check "policy installs in one transaction" apply
check "the main table's subnets are direct" in_router nft get element inet cpxy direct_main '{ 192.0.2.7 }'
check "other devices are exempt" in_router nft get element inet cpxy exempt '{ "tail0r" }'
check_not "the uplink is not exempt" in_router nft get element inet cpxy exempt '{ "wan0r" }'

bg ip netns exec router env SERVER='http://:lab@[2001:db8:2::1]:8443' SOCKS5_PROXY_LISTEN=127.0.0.1:1080 \
	TRANSPARENT_PROXY_LISTEN=0.0.0.0:5100 "$BIN/cpxy" client >"$WORK/client.log" 2>&1
CLIENT_PID=$!
bg ip netns exec router "$BIN/cpxy" dns --resolver-routing cn_split --socks5-proxy 127.0.0.1:1080 \
	--listen '[::]:5335' --upstream 114.114.114.114 --alternative 8.8.8.8 >"$WORK/dns.log" 2>&1
DNS_PID=$!
sleep 1

echo "--- running"
mark; check "overseas TCP reaches the web server" fetch
expect_seen "overseas TCP went through the cpxy server, to its original destination" 93.184.216.34
check "the transparent listener accepted the LAN connection" grep -q "Accepted connection addr=192.168.8.50:" "$WORK/client.log"
mark; check "CN TCP reaches the web server" fetch "$CN_URL"
expect_seen_direct "CN TCP is forwarded directly by the kernel" 192.0.2.2
check "the router itself is reachable, not redirected" in_lan curl -fsS --max-time 3 http://192.168.8.1:8080/index.html
check "a tailnet-like peer is reachable" in_lan curl -fsS --max-time 3 http://100.100.1.1:8000/index.html
check "a tailnet-like peer is reached directly through its device" grep -q '^192.168.8.50 ' "$WORK/tail.log"
check_not "a direct connection to the transparent listener is refused" in_lan curl -fsS --max-time 3 http://192.168.8.1:5100/
check "cpxy-client survives the refused connection" kill -0 "$CLIENT_PID"
mark; check "router's own traffic still goes out WAN" in_router curl -sS --max-time 5 --noproxy '*' "$URL"
expect_seen "router traffic is not captured" 192.0.2.2
check "LAN IPv4 ping goes directly to the internet" in_lan ping -c1 -W2 93.184.216.34
check_not "LAN IPv6 ping is refused" in_lan ping -6 -c1 -W2 2001:db8:100::34
got="$(udp_probe 443)"
[ "$got" = refused ] && pass "QUIC (UDP 443) gets an immediate refusal" || fail "UDP 443 was not refused (got: $got)"
got="$(udp_probe 9999)"
[ "$got" = "reply from 192.0.2.2" ] && pass "other UDP goes out the WAN directly ($got)" ||
	fail "UDP 9999 did not go out the WAN directly (got: $got)"
check_not "overseas IPv6 TCP is refused" fetch "$URL_V6"
check_not "regional IPv6 TCP is refused" fetch 'http://[2400:3200::114]:8000/index.html'
for port in 443 9999; do
	got="$(udp_probe "$port" 2001:db8:100::34)"
	[ "$got" = refused ] && pass "IPv6 UDP $port is refused" || fail "IPv6 UDP $port was not refused: $got"
done
check "router-originated IPv6 still works" in_router curl -gfsS --max-time 3 --noproxy '*' "$URL_V6"

# --- Split DNS on UDP and TCP 53, through the DNS-only worker ---
[ "$(dns_query 8.8.4.4 cn.test)" = 'NOERROR 114.114.114.114' ] && pass "CN DNS answer over UDP" || fail "CN DNS answer"
[ "$(dns_query 8.8.4.4 overseas.test tcp)" = 'NOERROR 93.184.216.34' ] && pass "TCP/53 is redirected to split DNS, not the proxy" || fail "TCP DNS"
[ "$(dns_query 8.8.4.4 overseas.test udp AAAA)" = NOERROR ] && pass "AAAA is empty" || fail "AAAA not empty"
[ "$(dns_query 2001:4860:4860::8844 overseas.test udp AAAA)" = NOERROR ] && pass "IPv6 DNS is redirected too" || fail "IPv6 DNS"
check "the overseas resolver was asked through cpxy-client" grep -q '8.8.8.8' "$WORK/client.log"

# --- A long-lived proxied connection survives a policy change ---
in_lan python3 -c '
import socket, sys, time
s = socket.create_connection(("93.184.216.34", 7000), timeout=5)
s.sendall(b"a"); assert s.recv(1) == b"a"
time.sleep(3)
s.sendall(b"b"); assert s.recv(1) == b"b"
print("echoed across the reload")
' >"$WORK/echo.out" 2>&1 &
ECHO=$!
sleep 1
check "a changed proxy_route replaces the policy" reinstall main 5100 5335 lan0r 114.0.0.0/8
if wait "$ECHO"; then pass "an established proxied connection survives the reload"; else fail "the established connection broke"; cat "$WORK/echo.out"; fi
mark; check "forced CN destination is reachable" fetch "$CN_URL"
expect_seen_direct "forced CN destination now goes through the cpxy server" 114.114.114.114
check "restoring the automatic policy" reinstall main 5100 5335 lan0r
mark; check "CN TCP is direct again" fetch "$CN_URL"
expect_seen_direct "CN TCP direct after the restore" 192.0.2.2
check "the whole dataset as proxy routes leaves an empty set" reinstall main 5100 5335 lan0r 0.0.0.0/0
mark; check "with an empty direct set CN TCP is proxied" fetch "$CN_URL"
expect_seen_direct "empty direct set proxies CN TCP" 114.114.114.114
instance main 5100 5335 lan0r && apply

# --- A second network on its own server ---
check "guest instance policy installs" reinstall guest 5101 5345 guest0r
bg ip netns exec router env SERVER=http://:guestlab@192.0.3.1:8444 SOCKS5_PROXY_LISTEN=127.0.0.1:1081 \
	TRANSPARENT_PROXY_LISTEN=0.0.0.0:5101 "$BIN/cpxy" client >"$WORK/guest-client.log" 2>&1
GUEST_PID=$!
sleep 1
mark; check "guest uses the second upstream" guest_fetch
expect_seen "guest upstream has its own source address" 192.0.3.1
mark; check "main still uses the first upstream" fetch
expect_seen "main upstream unchanged" 93.184.216.34
kill "$GUEST_PID"; sleep 0.5
mark; check_not "guest client down: guest TCP is refused" guest_fetch
expect_not_direct "guest client down: nothing leaked via the WAN"
check "guest client down: guest IPv4 ping stays direct" in_guest ping -c1 -W2 93.184.216.34
mark; check "guest failure leaves main working" fetch
expect_seen "main upstream after guest failure" 93.184.216.34
rm -f "$PLAN/guest" "$PLAN/routes/guest"
check "guest removal reapplies" apply
mark; check "guest removed: guest goes direct" guest_fetch
expect_seen "guest direct after removal" 192.0.2.2

# --- Fail closed ---
kill "$CLIENT_PID"; sleep 0.5
mark; check_not "cpxy-client down: overseas TCP is refused" fetch
expect_not_direct "cpxy-client down: nothing leaked via the WAN"
check "cpxy-client down: CN TCP still works" fetch "$CN_URL"
bg ip netns exec router env SERVER='http://:lab@[2001:db8:2::1]:8443' SOCKS5_PROXY_LISTEN=127.0.0.1:1080 \
	TRANSPARENT_PROXY_LISTEN=0.0.0.0:5100 "$BIN/cpxy" client >>"$WORK/client.log" 2>&1
CLIENT_PID=$!
sleep 0.5
mark; check "cpxy-client restart restores proxied TCP" fetch
expect_seen "restarted client uses cpxy" 93.184.216.34
# A missing redirect, as before the table is complete: the guard refuses instead of forwarding.
in_router nft flush chain inet cpxy dstnat
mark; check_not "redirect missing: overseas TCP is refused by the guard" fetch
expect_not_direct "redirect missing: nothing leaked via the WAN"
check "redirect missing: CN TCP still works" fetch "$CN_URL"
check "reapplying restores the redirect" apply
mark; check "reapplied: overseas TCP proxied again" fetch
expect_seen "reapplied: through cpxy" 93.184.216.34
kill "$SERVER_PID"; sleep 0.5
mark; check_not "server down: overseas TCP fails closed" fetch
expect_not_direct "server down: no direct fallback"
check "server down: CN TCP still works" fetch "$CN_URL"
kill -TERM "$DNS_PID"
if wait "$DNS_PID"; then pass "DNS worker stops cleanly"; else fail "DNS worker stop returned an error"; fi
check_not "nothing panicked" grep -m3 panicked "$WORK/client.log" "$WORK/dns.log"

# --- Stop: the original setup is back ---
check "stop removes the table" in_router sh -c ". '$NFT'; cpxy_nft_clear; ! nft list table inet cpxy"
mark; check "stop: LAN reaches the internet directly again" fetch
expect_seen "stop: direct again via the WAN" 192.0.2.2

if [ "$FAILED" = 0 ]; then
	echo "lab passed"
else
	echo "lab FAILED"
	for f in client guest-client dns server cn-dns overseas-dns; do echo "== $f.log"; tail -n 15 "$WORK/$f.log" 2>/dev/null; done
fi
exit "$FAILED"
