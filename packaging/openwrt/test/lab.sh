#!/bin/sh
# Routing lab: checks the policy routing and fail-closed behaviour with the real binaries, in
# throwaway network namespaces (no root needed; uses an unprivileged user namespace).
#
#   lan (192.168.8.50) -- router (LAN 192.168.8.1, WAN 192.0.2.2, masquerading) -- inet
#   inet: a cpxy server on 192.0.2.1:8443 and a web server on 93.184.216.34:8000
#
# The router mimics OpenWrt: strict rp_filter, forward policy drop, and the package's real
# routing code (net.sh), tun2proxy wrapper and nftables snippet. It does not run fw4 or dnsmasq.
#
# usage: lab.sh <dir with cpxy-client, cpxy-server and cpxy-tun2proxy>
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
	for p in $PIDS $(for ns in inet router lan; do ip netns pids "$ns" 2>/dev/null; done); do kill "$p" 2>/dev/null; done
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
for ns in inet router lan; do ip netns add "$ns"; done
ip link add wan0 type veth peer name wan0r
ip link set wan0 netns inet; ip link set wan0r netns router
ip link add lan0r type veth peer name lan0
ip link set lan0r netns router; ip link set lan0 netns lan

in_inet() { ip netns exec inet "$@"; }
in_router() { ip netns exec router "$@"; }
in_lan() { ip netns exec lan "$@"; }

in_inet sh -c 'ip link set lo up; ip addr add 93.184.216.34/32 dev lo; ip addr add 192.0.2.1/24 dev wan0; ip link set wan0 up
	ip route add 192.168.8.0/24 via 192.0.2.2'
in_router sh -c 'ip link set lo up; ip addr add 192.0.2.2/24 dev wan0r; ip addr add 192.168.8.1/24 dev lan0r
	ip link set wan0r up; ip link set lan0r up; ip route add default via 192.0.2.1
	sysctl -qw net.ipv4.ip_forward=1 net.ipv4.conf.all.rp_filter=1 net.ipv4.conf.default.rp_filter=1'
in_lan sh -c 'ip link set lo up; ip addr add 192.168.8.50/24 dev lan0; ip link set lan0 up; ip route add default via 192.168.8.1'

# Router firewall, shaped like fw4: forward drops by default, the package snippet comes first,
# LAN may go to WAN (the original setup) and to the TUN device (the cpxy zone).
cat >"$WORK/fw.nft" <<EOF
table inet fw4 {
	chain forward {
		type filter hook forward priority filter; policy drop;
		include "$PKG/usr/share/nftables.d/chain-pre/forward/10-cpxy.nft"
		ct state established,related accept
		iifname "lan0r" oifname { "wan0r", "cpxy0" } accept
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
bg in_inet "$BIN/cpxy-server" --key lab 192.0.2.1:8443 2>"$WORK/server.log"
(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind 93.184.216.34 2>"$WORK/web.log")
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
bg in_router env SERVER=http://:lab@192.0.2.1:8443 "$BIN/cpxy-client" --socks5-proxy-listen 127.0.0.1:1080 \
	--api-listen 127.0.0.1:3010 --dns-server 192.0.2.1 2>"$WORK/client.log"
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_up lan0r"
check "policy routing installs" in_router ip rule show
# Not via in_router: `ip netns exec` execs, so $! is tun2proxy itself (pgrep would also match
# processes outside the lab)
bg ip netns exec router "$BIN/cpxy-tun2proxy" --proxy socks5://127.0.0.1:1080 --tun cpxy0 --dns direct --exit-on-fatal-error 2>"$WORK/tun.log"
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
# A reload runs cpxy_net_up again while tun2proxy keeps running; it must stay attached to cpxy0
in_router sh -c ". '$PKG/usr/libexec/cpxy/net.sh'; cpxy_net_up lan0r"
mark; check "reload: LAN still reaches the web server through the proxy" fetch
got="$(seen)"
if [ -n "$got" ] && [ "$got" != 192.0.2.2 ]; then pass "reload: still through cpxy"
else fail "reload: request did not go through the proxy (web server saw: '${got}')"; fi
mark; check "router's own traffic still goes out WAN" in_router curl -sS --max-time 5 --noproxy '*' "$URL"
expect_seen "router traffic is not captured" 192.0.2.2
check "LAN can still reach the router itself" in_lan ping -c1 -W2 192.168.8.1
in_lan sh -c 'timeout 3 python3 - <<PY
import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2); s.connect(("93.184.216.34",9999)); s.send(b"x")
try: s.recv(10)
except ConnectionRefusedError: print("refused"); sys.exit(0)
except socket.timeout: print("timeout"); sys.exit(1)
PY' >"$WORK/out" 2>&1 && pass "UDP gets an immediate ICMP refusal" || { fail "UDP was not refused (got: $(cat "$WORK/out"))"; }

# --- Fail closed: tun2proxy dies ---
kill "$TUN_PID"; sleep 1
mark; check_not "tun2proxy down: LAN traffic is refused" fetch
expect_not_direct "tun2proxy down: nothing leaked via the WAN"

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
	for f in client tun server; do echo "== $f.log"; tail -n 15 "$WORK/$f.log" 2>/dev/null; done
fi
exit "$FAILED"
