#!/bin/sh
# Engine lab: runs mobile-engine on a real TUN device against a local cpxy server and checks the
# routing, DNS split, refusals and fail-closed behaviour, in throwaway network namespaces (no root
# needed; uses an unprivileged user namespace).
#
#   apps (tun0 10.233.0.1, default route) ~~TUN fd~~ phone (engine, 192.0.2.2) -- inet
#   inet: cpxy server 192.0.2.1:8443, web servers on 93.184.216.34 (not CN) and
#         114.114.114.114 (CN), DNS servers 192.0.2.53 (upstream) and 192.0.2.54 (alternative)
#
# The engine creates tun0 in the phone namespace and the lab moves it to the apps namespace, so
# the engine's own sockets leave through the phone's uplink: on Android, the app is excluded from
# its own VPN the same way.
#
# usage: tun-lab.sh <dir with cpxy-server and mobile-engine-linux>
set -u

if [ -z "${CPXY_LAB_INNER:-}" ]; then
	exec env CPXY_LAB_INNER=1 unshare -Urmn --propagation private sh "$0" "$@"
fi

BIN="$(cd "$1" && pwd)"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
FAILED=0
PIDS=""
cleanup() {
	for p in $PIDS $(for ns in inet phone apps; do ip netns pids "$ns" 2>/dev/null; done); do kill "$p" 2>/dev/null; done
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
expect() { # expect <name> <wanted> <got>
	if [ "$3" = "$2" ]; then pass "$1 ($3)"; else fail "$1 (got '$3', wanted '$2')"; fi
}
bg() { "$@" & PIDS="$PIDS $!"; }

mount -t tmpfs tmpfs /run && mkdir -p /run/netns
for ns in inet phone apps; do ip netns add "$ns"; done
ip link add wan0 type veth peer name wan0p
ip link set wan0 netns inet; ip link set wan0p netns phone

in_inet() { ip netns exec inet "$@"; }
in_phone() { ip netns exec phone "$@"; }
in_apps() { ip netns exec apps "$@"; }

in_inet sh -c 'ip link set lo up; ip addr add 93.184.216.34/32 dev lo; ip addr add 114.114.114.114/32 dev lo
	ip addr add 192.0.2.1/24 dev wan0; ip addr add 192.0.2.53/24 dev wan0; ip addr add 192.0.2.54/24 dev wan0
	ip link set wan0 up'
in_phone sh -c 'ip link set lo up; ip addr add 192.0.2.2/24 dev wan0p; ip link set wan0p up; ip route add default via 192.0.2.1'
in_apps ip link set lo up

# --- Internet side ---
mkdir "$WORK/www" && echo hello >"$WORK/www/index.html"
head -c 4000000 /dev/urandom >"$WORK/www/big.bin"
# Not via in_inet: `ip netns exec` execs, so $! is the server itself
ip netns exec inet "$BIN/cpxy-server" --key lab 192.0.2.1:8443 >"$WORK/server.log" 2>&1 &
PIDS="$PIDS $!"
SERVER_PID=$!
for ip in 93.184.216.34 114.114.114.114; do
	(cd "$WORK/www" && bg in_inet python3 -m http.server 8000 --bind "$ip" 2>>"$WORK/web.log")
done
# Listens on 853 so that a refusal there can only come from the engine
bg in_inet python3 -m http.server 853 --bind 93.184.216.34 2>/dev/null
# The upstream server answers first; for foreign.test its answer is poisoned (not in CN)
bg in_inet python3 "$HERE/dns.py" serve 192.0.2.53 0 cn.test=114.114.114.114 foreign.test=198.51.100.1
bg in_inet python3 "$HERE/dns.py" serve 192.0.2.54 300 cn.test=93.184.216.34 foreign.test=93.184.216.34
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

# --- The engine, standing in for the phone ---
cat >"$WORK/config.json" <<EOF
{
  "server": "http://:lab@192.0.2.1:8443",
  "dns_upstream": ["192.0.2.53"],
  "dns_alternative": ["tcp://192.0.2.54:53"]
}
EOF
ip netns exec phone env RUST_LOG="${RUST_LOG:-info}" "$BIN/mobile-engine-linux" tun0 "$WORK/config.json" >"$WORK/engine.log" 2>&1 &
ENGINE_PID=$!
PIDS="$PIDS $ENGINE_PID"
for _ in $(seq 50); do in_phone ip link show tun0 >/dev/null 2>&1 && break; sleep 0.1; done
check "engine creates its TUN device" in_phone ip link set tun0 netns apps
# What the platform does: the TUN address, a default route for IPv4 and IPv6, and the MTU
in_apps sh -c 'sysctl -qw net.ipv6.conf.all.disable_ipv6=0 net.ipv6.conf.default.disable_ipv6=0
	ip addr add 10.233.0.1/30 dev tun0; ip -6 addr add fd00:1::1/64 dev tun0 nodad
	ip link set tun0 mtu 1500 up; ip route add default dev tun0; ip -6 route add default dev tun0'

echo "--- running"
URL_PROXIED=http://93.184.216.34:8000
URL_CN=http://114.114.114.114:8000
fetch() { in_apps curl -sS --max-time 5 --noproxy '*' "$1/index.html"; }
# Who did the web servers see since mark? 192.0.2.2 is the phone's uplink: traffic that went direct
mark() { MARK="$(wc -l <"$WORK/web.log")"; }
seen() { sleep 0.3; tail -n +"$((MARK + 1))" "$WORK/web.log" | grep -o '^[0-9.]*' | sort -u | tr '\n' ' ' | sed 's/ $//'; }

# DNS split
dns() { in_apps python3 "$HERE/dns.py" query 10.233.0.2 "$@"; }
expect "DNS: a CN answer from upstream wins" "NOERROR 114.114.114.114" "$(dns cn.test)"
expect "DNS: a non-CN upstream answer loses to the alternative" "NOERROR 93.184.216.34" "$(dns foreign.test)"
expect "DNS over TCP" "NOERROR 93.184.216.34" "$(dns foreign.test tcp)"
expect "DNS: AAAA is answered empty" "NOERROR" "$(dns cn.test udp AAAA)"
expect "DNS: unknown names are NXDOMAIN" "NXDOMAIN" "$(dns nope.test)"

# TCP routing
mark; check "CN site is reachable" fetch "$URL_CN"
expect "CN site goes direct" 192.0.2.2 "$(seen)"
mark; check "non-CN site is reachable" fetch "$URL_PROXIED"
got="$(seen)"
if [ -n "$got" ] && [ "$got" != 192.0.2.2 ]; then pass "non-CN site goes through cpxy (web server saw: $got)"
else fail "non-CN site did not go through the proxy (web server saw: '${got}')"; fi

# Bulk transfers in parallel, both ways round, then the engine's peak memory
want="$(sha256sum <"$WORK/www/big.bin")"
BULK=""
for i in $(seq 8); do
	for url in "$URL_PROXIED" "$URL_CN"; do
		in_apps sh -c "curl -sS --max-time 60 --noproxy '*' '$url/big.bin' | sha256sum" >"$WORK/bulk.$i.${url#http://}" 2>&1 &
		BULK="$BULK $!"
	done
done
# shellcheck disable=SC2086
wait $BULK
bulk_intact() { for f in "$WORK"/bulk.*; do [ "$(cat "$f")" = "$want" ] || { echo "$f: $(cat "$f")"; return 1; }; done; }
check "16 parallel 4 MB downloads arrive intact" bulk_intact
peak_kb="$(awk '/^VmHWM/ {print $2}' "/proc/$ENGINE_PID/status")"
echo "     engine peak RSS: $((peak_kb / 1024)) MB"
[ "$peak_kb" -lt 51200 ] && pass "engine stays under the iOS extension's 50 MB" || fail "engine peak RSS is ${peak_kb} kB"

# UDP. udp_probe <host> <port>: what a UDP datagram from the apps to the echo server got back
udp_probe() {
	in_apps sh -c "timeout 4 python3 - $1 $2 <<'PY'
import socket,sys
s=socket.socket(socket.AF_INET6 if ':' in sys.argv[1] else socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(2)
s.connect((sys.argv[1],int(sys.argv[2]))); s.send(b'x')
try: print('reply from', s.recv(64).decode())
except ConnectionRefusedError: print('refused')
except socket.timeout: print('timeout')
except PermissionError: print('prohibited')
except OSError as e: print(e)
PY"
}
# The echo server answers on 443 too
expect "QUIC (UDP 443) gets an immediate ICMP refusal" refused "$(udp_probe 93.184.216.34 443)"
expect "other UDP goes direct" "reply from 192.0.2.2" "$(udp_probe 93.184.216.34 9999)"

# tcp_probe <host> <port>: how a TCP connect from the apps ends, within 2 seconds
tcp_probe() {
	in_apps python3 - "$1" "$2" <<'PY'
import socket,sys
try: socket.create_connection((sys.argv[1],int(sys.argv[2])),timeout=2); print('connected')
except ConnectionRefusedError: print('refused')
except socket.timeout: print('timeout')
except PermissionError: print('prohibited')
except OSError as e: print(e)
PY
}
expect "DNS over TLS (TCP 853) is refused" refused "$(tcp_probe 93.184.216.34 853)"
# The packets leave (the route exists) and the engine's ICMPv6 error comes back
expect "IPv6 UDP is refused" prohibited "$(udp_probe 2001:db8::1 9999)"
expect "IPv6 TCP is refused" prohibited "$(tcp_probe 2001:db8::1 443)"

# --- Fail closed: the cpxy server dies ---
kill "$SERVER_PID"; sleep 0.5
mark; check_not "server down: non-CN site is unreachable" fetch "$URL_PROXIED"
expect "server down: nothing leaked direct" "" "$(seen)"
mark; check "server down: CN site still works" fetch "$URL_CN"

# --- Stop ---
kill -TERM "$ENGINE_PID"
for _ in $(seq 50); do kill -0 "$ENGINE_PID" 2>/dev/null || break; sleep 0.1; done
if kill -0 "$ENGINE_PID" 2>/dev/null; then fail "engine did not stop within 5 seconds"
elif wait "$ENGINE_PID"; then pass "engine stops cleanly"
else fail "engine exited with an error"; fi
check_not "stop: the TUN device is gone" in_apps ip link show tun0
# A panic on an engine thread does not change the exit code
check_not "engine never panicked" grep -m3 panicked "$WORK/engine.log"

if [ "$FAILED" = 0 ]; then
	echo "lab passed"
else
	echo "lab FAILED"
	for f in engine server; do echo "== $f.log"; tail -n 30 "$WORK/$f.log" 2>/dev/null; done
fi
exit "$FAILED"
