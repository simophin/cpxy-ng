#!/bin/bash
# The phone (c) runs rtp-mobile-linux on a TUN that routes everything but the direct set, against
# a cpxy-rtp internet-exit endpoint (e). b is "the internet": an echo server and the alternative
# DNS server, which only answer the tunnel pool. l is a local-region host: an echo server and the
# upstream DNS server, which only answer the phone's own network.
#
#   unshare -Urnm bash rtp-mobile/tests/linux-lab.sh
#
# RTP_SERVICE_BIN and RTP_MOBILE_BIN default to the debug builds.
set -euo pipefail
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ]; then echo 'Run inside unshare -Urnm' >&2; exit 1; fi
here=$(cd "$(dirname "$0")" && pwd)
dns="$here/../../mobile-engine/test/dns.py"
service=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
mobile=$(realpath "${RTP_MOBILE_BIN:-target/debug/rtp-mobile-linux}")
mount --make-rprivate /
mount -t tmpfs tmpfs /run
for n in c e b l; do ip netns add "$n"; ip -n "$n" link set lo up; done
pair() {
 ip link add "$1" type veth peer name "$2"
 ip link set "$1" netns "$3"; ip link set "$2" netns "$4"
 ip -n "$3" link set "$1" up; ip -n "$4" link set "$2" up
}
pair cr ec c e; pair eb be e b; pair el le e l
ip -n c addr add 192.0.2.1/24 dev cr
ip -n c route add default via 192.0.2.254
ip -n e addr add 192.0.2.254/24 dev ec
ip -n e addr add 172.31.1.1/30 dev eb
ip -n e addr add 172.31.2.1/30 dev el
ip -n b addr add 172.31.1.2/30 dev be
ip -n b addr add 8.8.8.8/32 dev lo
ip -n b addr add 203.0.113.2/32 dev lo
ip -n l addr add 172.31.2.2/30 dev le
ip -n l addr add 223.5.5.5/32 dev lo
ip -n e route add 8.8.8.8/32 via 172.31.1.2
ip -n e route add 203.0.113.2/32 via 172.31.1.2
ip -n e route add 223.5.5.5/32 via 172.31.2.2
ip -n b route add 10.88.0.0/24 via 172.31.1.1
ip -n l route add 192.0.2.0/24 via 172.31.2.1
ip netns exec e python3 -c 'open("/proc/sys/net/ipv4/ip_forward", "w").write("1")'

work=$(mktemp -d /tmp/rtp-mobile-XXXX)
export CPXY_RTP_PASSWORD="mobile-fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; for p in "${pids[@]}"; do wait "$p" 2>/dev/null || true; done; }
trap 'status=$?; if [ "$status" -ne 0 ]; then tail -n 30 "$work"/*.log 2>/dev/null || true; fi; cleanup; rm -rf "$work"; exit "$status"' EXIT

cat > "$work/echo.py" <<'PY'
import socket, sys, threading
ip = sys.argv[1]
def tcp():
    s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind((ip, 8080)); s.listen()
    while True:
        c, a = s.accept(); c.recv(100); c.sendall(a[0].encode()); c.close()
threading.Thread(target=tcp, daemon=True).start()
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); u.bind((ip, 8080))
while True:
    d, a = u.recvfrom(100); u.sendto(a[0].encode(), a)
PY
# app.py IP KIND: prints the address the server saw, over TCP or UDP.
cat > "$work/app.py" <<'PY'
import socket, sys
kind = {"tcp": socket.SOCK_STREAM, "udp": socket.SOCK_DGRAM}[sys.argv[2]]
port = int(sys.argv[3]) if len(sys.argv) > 3 else 8080
s = socket.socket(socket.AF_INET, kind); s.settimeout(3); s.connect((sys.argv[1], port)); s.send(b"hi")
print(s.recv(100).decode())
PY
ip netns exec b python3 "$work/echo.py" 203.0.113.2 >"$work/b-echo.log" 2>&1 & pids+=($!)
ip netns exec l python3 "$work/echo.py" 223.5.5.5 >"$work/l-echo.log" 2>&1 & pids+=($!)
ip netns exec b python3 "$dns" serve 8.8.8.8 0 remote.test=203.0.113.2 local.test=203.0.113.3 >"$work/b-dns.log" 2>&1 & pids+=($!)
ip netns exec l python3 "$dns" serve 223.5.5.5 0 remote.test=203.0.113.9 local.test=223.5.5.5 >"$work/l-dns.log" 2>&1 & pids+=($!)
ip netns exec e "$service" endpoint --internet-exit --listen 192.0.2.254:30000 --interface rtpe \
  --firewall none --state-dir "$work/e" >"$work/e.out" 2>"$work/e.log" & endpoint=$!; pids+=($endpoint)

# What VpnService does on the phone: a TUN with the engine's routes and an IPv6 catch-all.
ip -n c tuntap add rtpc mode tun
ip -n c addr add 10.88.0.2/32 dev rtpc
ip -n c link set rtpc mtu 1200 up
"$mobile" routes | sed 's/^/route add /; s/$/ dev rtpc/' > "$work/routes"
[ "$(wc -l < "$work/routes")" -gt 1000 ]
ip -n c -batch "$work/routes"
cat > "$work/profile.json" <<JSON
{"endpoint":"192.0.2.254:30000","dns_upstream":["223.5.5.5"],"dns_alternative":["8.8.8.8"]}
JSON
mkdir "$work/control"
ip netns exec c "$mobile" run "$work/profile.json" rtpc cr "$work/control" >"$work/c.out" 2>"$work/c.log" & phone=$!; pids+=($phone)
for i in $(seq 1 100); do grep -q '"state":"connected"' "$work/c.out" && break; sleep .1; done
grep -q '"state":"connected"' "$work/c.out"
app() { ip netns exec c python3 "$work/app.py" "$@"; }

# Tunnelled traffic reaches the internet from the tunnel address; direct traffic does not use it.
[ "$(app 203.0.113.2 tcp)" = 10.88.0.2 ]
[ "$(app 203.0.113.2 udp)" = 10.88.0.2 ]
[ "$(app 223.5.5.5 tcp)" = 192.0.2.1 ]
echo 'tunnel and direct routing ok'

# DNS split: the local-region answer from upstream, the other through the tunnel.
ip netns exec c python3 "$dns" query 198.18.0.53 local.test | grep -q '223.5.5.5'
ip netns exec c python3 "$dns" query 198.18.0.53 remote.test | grep -q '203.0.113.2'
# The tunnel carries no IPv6, so no AAAA answers.
if ip netns exec c python3 "$dns" query 198.18.0.53 remote.test udp AAAA | grep -q ':'; then exit 1; fi
echo 'DNS split ok'

# QUIC and DNS over TLS are refused at once, not dropped.
if out=$(app 203.0.113.2 udp 443 2>&1); then echo "QUIC was not refused: $out"; exit 1; fi
grep -q 'Connection refused' <<<"$out"
if out=$(app 203.0.113.2 tcp 853 2>&1); then echo "DoT was not refused: $out"; exit 1; fi
grep -q 'Connection refused' <<<"$out"
echo 'refusals ok'

# A new network: the phone's address changes and the platform reports it.
ip -n c addr flush dev cr
ip -n c addr add 192.0.2.7/24 dev cr
ip -n c route add default via 192.0.2.254
touch "$work/control/repair"
for i in $(seq 1 30); do [ ! -e "$work/control/repair" ] && break; sleep .1; done
# Repaired at once, the first SYN retransmit (1 s) gets through. Waiting for the worker's own
# liveness PING instead takes over 2 s.
start=$(date +%s%N)
[ "$(app 203.0.113.2 tcp)" = 10.88.0.2 ]
elapsed=$(( ($(date +%s%N) - start) / 1000000 ))
[ "$elapsed" -lt 1600 ] || { echo "Recovery took ${elapsed} ms"; exit 1; }
[ "$(app 223.5.5.5 tcp)" = 192.0.2.7 ]
echo 'network change ok'

# Fail closed: without the endpoint, tunnelled traffic fails instead of going direct.
kill -KILL "$endpoint"; wait "$endpoint" 2>/dev/null || true
if app 203.0.113.2 tcp >/dev/null 2>&1; then echo 'traffic escaped the dead tunnel'; exit 1; fi
[ "$(app 223.5.5.5 tcp)" = 192.0.2.7 ]
echo 'fail closed ok'

touch "$work/control/stop"
wait "$phone"
tail -1 "$work/c.out" | grep -q '"state":"stopped"'
echo RTP_MOBILE_LAB_OK
