#!/bin/bash
# Full-exit gateway regression: NAT, TCP/UDP, router bypass, crash guard.
set -euo pipefail
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ]; then echo 'Run inside unshare -Urnm' >&2; exit 1; fi
mount --make-rprivate /
mount -t tmpfs tmpfs /run
for n in c e b l; do ip netns add "$n"; ip -n "$n" link set lo up; done
pair() {
 ip link add "$1" type veth peer name "$2"
 ip link set "$1" netns "$3"; ip link set "$2" netns "$4"
 ip -n "$3" link set "$1" up; ip -n "$4" link set "$2" up
}
pair vc ve c e; pair eb be e b; pair cl lc c l
ip -n c addr add 192.0.2.1/24 dev vc
ip -n e addr add 192.0.2.2/24 dev ve
ip -n e addr add 203.0.113.1/24 dev eb
ip -n b addr add 203.0.113.2/24 dev be
ip -n c addr add 192.168.20.1/24 dev cl
ip -n l addr add 192.168.20.2/24 dev lc
ip -n c route add default via 192.0.2.2
ip -n l route add default via 192.168.20.1
ip -n b route add default via 203.0.113.1
for n in c e; do ip netns exec "$n" python3 -c 'open("/proc/sys/net/ipv4/ip_forward", "w").write("1")'; done
ip -n c route add unreachable default table 18801 metric 32760
ip -n c rule add priority 810 iif cl lookup 18801
ip -n c rule add priority 812 iif cl prohibit
ip netns exec c nft -f - <<'NFT'
table ip fixture {
 chain nat { type nat hook postrouting priority srcnat; oifname "rtpc" masquerade; }
 chain guard { type filter hook forward priority filter; iifname "cl" oifname != "rtpc" reject; }
}
NFT
work=$(mktemp -d /tmp/rtp-internet-XXXX)
export CPXY_RTP_PASSWORD="internet-fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
server=''; client=''; backend=''
cleanup(){ for pid in "$server" "$client" "$backend"; do if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi; done; }
trap 'status=$?; if [ "$status" -ne 0 ]; then cat "$work"/*.log 2>/dev/null || true; fi; cleanup; rm -rf "$work"; exit "$status"' EXIT
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
cat > "$work/backend.py" <<'PY'
import socket, threading

def tcp():
 s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('203.0.113.2',8080)); s.listen()
 while True:
  c,a=s.accept(); c.recv(100); c.sendall(a[0].encode()); c.close()
threading.Thread(target=tcp,daemon=True).start()
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(('203.0.113.2',8080))
while True:
 d,a=s.recvfrom(100); s.sendto(a[0].encode(),a)
PY
cat > "$work/app.py" <<'PY'
import socket,sys
for kind in [socket.SOCK_STREAM,socket.SOCK_DGRAM]:
 s=socket.socket(socket.AF_INET,kind);s.settimeout(3);s.connect(('203.0.113.2',8080));s.send(b'test');assert s.recv(100).decode()==sys.argv[1];s.close()
PY
ip netns exec b python3 "$work/backend.py" >"$work/backend.log" 2>&1 & backend=$!
ip netns exec e "$bin" endpoint --internet-exit --listen 192.0.2.2:30000 --interface rtpe --firewall none --state-dir "$work/e" >"$work/e.out" 2>"$work/e.log" & server=$!
ip netns exec c "$bin" client --internet-exit --entry 192.0.2.2:30000 --outbound-interface vc --interface rtpc --state-dir "$work/c" >"$work/c.out" 2>"$work/c.log" & client=$!
for i in $(seq 1 150); do grep -q '"event":"connected"' "$work/c.log" && break; sleep .1; done
grep -q '"event":"connected"' "$work/c.log" || { cat "$work/c.log" "$work/e.log"; exit 1; }
ip -n c route add default dev rtpc table 18801 metric 10
# Internet mode installs no global client capture rules.
[ "$(ip -n c rule show priority 1000)" = '' ]
ip netns exec c python3 "$work/app.py" 192.0.2.1
ip netns exec l python3 "$work/app.py" 10.88.0.2
# Neither process death nor loss of the capture rule can expose WAN fallback.
kill -KILL "$client"; wait "$client" 2>/dev/null || true; client=''
if ip -n c route get 203.0.113.2 from 192.168.20.2 iif cl; then echo 'crash exposed WAN'; exit 1; fi
if ip netns exec l python3 "$work/app.py" 192.0.2.1 >"$work/crash.log" 2>&1; then exit 1; fi
ip -n c rule del priority 810
if ip netns exec l python3 "$work/app.py" 192.0.2.1 >>"$work/crash.log" 2>&1; then exit 1; fi
ip netns exec c python3 "$work/app.py" 192.0.2.1
kill -TERM "$server"; wait "$server"; server=''
[ "$(ip -n e -j route show table 18800)" = '[]' ]
echo INTERNET_EXIT_OK
