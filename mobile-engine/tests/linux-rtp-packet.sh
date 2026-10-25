#!/bin/bash
set -euo pipefail
# Local disposable five-node fixture: clients c/d -> relay r -> endpoint e -> backend b.
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ];then echo 'Run in unshare -Urnm' >&2;exit 1;fi
mount --make-rprivate /
mount -t tmpfs tmpfs /run
for n in c d r e b;do ip netns add "$n";ip -n "$n" link set lo up;done
pair(){ ip link add "$1$2" type veth peer name "$2$1";ip link set "$1$2" netns "$1";ip link set "$2$1" netns "$2";ip -n "$1" addr add "$3" dev "$1$2";ip -n "$2" addr add "$4" dev "$2$1";ip -n "$1" link set "$1$2" up;ip -n "$2" link set "$2$1" up; }
pair c r 192.0.2.1/24 192.0.2.254/24
pair d r 198.51.100.1/24 198.51.100.254/24
ip -n d link set dr name cr
ip -n d route add 203.0.113.0/24 via 198.51.100.254
ip netns exec r python3 -c 'open("/proc/sys/net/ipv4/ip_forward","w").write("1")'
pair r e 203.0.113.1/24 203.0.113.2/24
pair e b 10.90.0.1/24 10.90.0.2/24
ip -n e route add 198.51.100.0/24 via 203.0.113.1
ip netns exec e python3 -c 'open("/proc/sys/net/ipv4/ip_forward","w").write("1")'
ip -n b route add 10.88.0.0/24 via 10.90.0.1
work=$(mktemp -d /tmp/rtp-a6-XXXX)
# The endpoint and the packet-API clients derive the same key from this password.
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
python3 - "$work" <<'PY'
import json,sys
from pathlib import Path
w=Path(sys.argv[1])
c={'mappings':[{'id':str(p),'generation':'fixed-1','listen':'0.0.0.0:'+str(p),'upstream':'203.0.113.2:'+str(p),'max_flows':128} for p in [30000,30001]],'max_flows':256,'max_source_flows':64,'max_payload':2048,'socket_buffer':32768,'idle_seconds':180,'packets_per_second':100000,'bytes_per_second':100000000,'mapping_packets_per_second':100000,'mapping_bytes_per_second':100000000,'source_packets_per_second':100000,'source_bytes_per_second':100000000}
json.dump(c,open(w/'relay.json','w'))
PY
cat > "$work/backend.py" <<'PY'
import socket,threading

def tcp():
 s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('10.90.0.2',8080));s.listen()
 while True:
  c,a=s.accept();d=c.recv(200);c.sendall(b'tcp:'+d);c.close()
threading.Thread(target=tcp,daemon=True).start()
u=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);u.bind(('10.90.0.2',8080))
while True:
 d,a=u.recvfrom(200);u.sendto(b'udp:'+d,a)
PY
cat > "$work/app.py" <<'PY'
import socket,sys
payload=sys.argv[1].encode()
for kind,prefix in [(socket.SOCK_STREAM,b'tcp:'),(socket.SOCK_DGRAM,b'udp:')]:
 s=socket.socket(socket.AF_INET,kind);s.settimeout(2);s.connect(('10.90.0.2',8080));s.send(payload);assert s.recv(200)==prefix+payload;s.close()
PY
packetbin=$(realpath "${RTP_PACKET_BIN:-target/debug/examples/rtp-packet}")
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
relaybin=$(realpath "${UDP_RELAY_BIN:-target/debug/cpxy-udp-relay}")
pids=();relay=''
cleanup(){ for p in "${pids[@]}" "$relay";do [ -z "$p" ] || kill "$p" 2>/dev/null || true;done;for p in "${pids[@]}" "$relay";do [ -z "$p" ] || wait "$p" 2>/dev/null || true;done; }
trap cleanup EXIT
ip netns exec b python3 "$work/backend.py" > "$work/backend.log" 2>&1 & pids+=($!)
start_relay(){ ip netns exec r "$relaybin" run "$work/relay.json" >> "$work/relay.out" 2>> "$work/relay.log" & relay=$!; }
start_relay
ip netns exec e env POC_TEST_DROP_ACTIVE_PATH_FILE="$work/drop" "$bin" endpoint --interface rtpe \
  --state-dir "$work/e" --listen 203.0.113.2:30000 --rotation-ports 30001-30008 --rotation-seconds 0 \
  --initial-rotation-port 30001 --keepalive-seconds 5 --service-cidr 10.90.0.0/24 \
  --client-address 10.88.0.2,10.88.0.3 --firewall none > "$work/e.out" 2> "$work/e.log" & pids+=($!)
clientpids=()
for n in c d;do
 addr=10.88.0.2;entry=192.0.2.254
 if [ "$n" = d ];then addr=10.88.0.3;entry=203.0.113.2;fi
 cat > "$work/$n-packet.json" <<JSON
{"entry":"$entry:30000","endpoint_identity":"fixture-endpoint","password_reference":"fixture-password","address":"$addr","routes":["10.90.0.0/24"],"dns":{"policy":"none"},"mtu":1200}
JSON
 ip -n "$n" tuntap add rtpc mode tun
 ip -n "$n" addr add "$addr/24" dev rtpc
 ip -n "$n" link set rtpc mtu 1200 up
 ip -n "$n" route add unreachable 10.90.0.0/24 metric 32760
 ip -n "$n" route add 10.90.0.0/24 dev rtpc metric 1
 ip netns exec "$n" "$packetbin" "$work/$n-packet.json" "$work/$n.stop" > "$work/$n.log" 2>&1 & pids+=($!);clientpids+=($!)
done
for i in $(seq 1 100);do if grep -Eq '"status":"connected"' "$work/c.log" && grep -Eq '"status":"connected"' "$work/d.log";then break;fi;sleep .1;done
apps(){ for n in c d;do ip netns exec "$n" python3 "$work/app.py" "$n-$1";done; }
apps initial
pingpids=()
for n in c d;do ip netns exec "$n" ping -c 35 -i .5 -W 1 10.90.0.2 > "$work/$n-ping.log" & pids+=($!);pingpids+=($!);done
sleep 1
touch "$work/drop"
# The endpoint hook blackholes one active session path; either client may be chosen.
# Repair can complete between polls; transitions retain the observed sequence.
repaired='"status":"connected".*"active_repairs":[1-9].*"transitions":\[[^]]*"repairing"[^]]*"connected"'
for i in $(seq 1 200);do if grep -Eq "$repaired" "$work/c.log" "$work/d.log";then break;fi;sleep .1;done
grep -Eq "$repaired" "$work/c.log" "$work/d.log"
for n in c d;do tail -1 "$work/$n.log" | grep -q '"status":"connected"';done
apps repaired
for p in "${pingpids[@]}";do wait "$p" || true;done
for n in c d;do touch "$work/$n.stop";done
for p in "${clientpids[@]}";do wait "$p";done
for n in c d;do grep -Eq '"status":"stopped"' "$work/$n.log";grep -Eq 'protected_sockets=([2-9]|[1-9][0-9]+)$' "$work/$n.log";done
# Initial + candidate sockets everywhere; the repaired client also opened repair sockets.
grep -Eq 'protected_sockets=([3-9]|[1-9][0-9]+)$' "$work/c.log" "$work/d.log"
# Explicit platform disconnect: persistent guards remained until this action.
for n in c d;do ip -n "$n" route del 10.90.0.0/24 dev rtpc;ip -n "$n" route del unreachable 10.90.0.0/24 metric 32760;ip -n "$n" tuntap del rtpc mode tun;done
echo "A6 direct+relayed application API, TCP/UDP, automatic repair and stop passed: $work"
