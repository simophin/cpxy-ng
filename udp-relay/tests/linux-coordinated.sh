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
pair r e 203.0.113.1/24 203.0.113.2/24
pair e b 10.90.0.1/24 10.90.0.2/24
ip netns exec e python3 -c 'open("/proc/sys/net/ipv4/ip_forward","w").write("1")'
ip -n b route add 10.88.0.0/24 via 10.90.0.1
work=$(mktemp -d /tmp/rtp-a5-XXXX)
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export CPXY_RTP_KEEPALIVE_SECONDS=5 CPXY_RTP_SERVICE_CIDRS=10.90.0.0/24
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$work/ca.key" -out "$work/ca.pem" -subj /CN=fixture-ca -days 1 -addext keyUsage=critical,keyCertSign,cRLSign 2>/dev/null
for name in relay controller;do
 openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout "$work/$name.key" -out "$work/$name.csr" -subj "/CN=$name" 2>/dev/null
 if [ "$name" = relay ];then usage=serverAuth;else usage=clientAuth;fi
 printf 'subjectAltName=DNS:relay\nextendedKeyUsage=%s\n' "$usage" > "$work/ext"
 openssl x509 -req -in "$work/$name.csr" -CA "$work/ca.pem" -CAkey "$work/ca.key" -CAcreateserial -out "$work/$name.pem" -days 1 -extfile "$work/ext" 2>/dev/null
done
python3 - "$work" <<'PY'
import json,sys
from pathlib import Path
w=Path(sys.argv[1])
c={'mappings':[{'id':str(p),'generation':'fixed-1','listen':'0.0.0.0:'+str(p),'upstream':'203.0.113.2:'+str(p),'max_flows':128} for p in [30000,30001]],'max_flows':256,'max_source_flows':64,'max_payload':2048,'socket_buffer':32768,'idle_seconds':180,'packets_per_second':100000,'bytes_per_second':100000000,'mapping_packets_per_second':100000,'mapping_bytes_per_second':100000000,'source_packets_per_second':100000,'source_bytes_per_second':100000000}
c['mappings']=[]
m={'relay':c,'management':'203.0.113.1:30200','tls':{'ca':str(w/'ca.pem'),'certificate':str(w/'relay.pem'),'private_key':str(w/'relay.key')},'authority':{'owner':'cpxy-rtp','listen_ip':'0.0.0.0','upstream_ip':'203.0.113.2','ports':list(range(30000,30009)),'max_flows':128},'journal':str(w/'relay.journal'),'agent_socket':str(w/'r-agent.sock'),'staged_seconds':15,'outage_seconds':120}
json.dump(m,open(w/'relay.json','w'))
import os
# Only the relay keeps a separate agent; the endpoint manages firewalld in-process.
for n,bind,interface,sources in [('r','0.0.0.0','rc',['192.0.2.0/24','198.51.100.0/24'])]:
 policy={'profile':'cpxy-rtp','bind':bind,'base':30000,'low':30001,'high':30008,'zone':'public','interface':interface,'sources':sources}
 json.dump({'socket':str(w/(n+'-agent.sock')),'journal':str(w/(n+'-agent.journal')),'allowed_uid':os.getuid(),'policies':[policy]},open(w/(n+'-agent.json'),'w'))
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
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
relaybin=$(realpath "${UDP_RELAY_BIN:-target/debug/cpxy-udp-relay}")
pids=();relay=''
cleanup(){ for p in "${pids[@]}" "$relay";do [ -z "$p" ] || kill "$p" 2>/dev/null || true;done;for p in "${pids[@]}" "$relay";do [ -z "$p" ] || wait "$p" 2>/dev/null || true;done; }
trap cleanup EXIT
agentbin=$(realpath target/debug/port-admission)
for n in r e;do
 mkdir -p "$work/$n-firewalld"
 cat > "$work/$n-bus.conf" <<BUS
<busconfig><type>system</type><listen>unix:path=/run/dbus-$n</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/><allow user="*"/></policy></busconfig>
BUS
 dbus-daemon --config-file="$work/$n-bus.conf" --fork
 ip netns exec "$n" env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-$n" /usr/sbin/firewalld --nofork --nopid --system-config "$work/$n-firewalld" --log-target console >"$work/$n-firewalld.log" 2>&1 & pids+=($!)
 for i in $(seq 1 100);do if ip netns exec "$n" env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-$n" firewall-cmd --state >/dev/null 2>&1;then break;fi;sleep .1;done
 if [ "$n" = r ];then ip netns exec "$n" env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-$n" "$agentbin" "$work/$n-agent.json" >"$work/$n-agent.log" 2>&1 & pids+=($!);fi
done
fw(){ local n=$1;shift;ip netns exec "$n" env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-$n" firewall-cmd "$@"; }
fw r --zone=public --add-port=30200/tcp
fw r --zone=public --add-port=39999/udp
fw e --zone=public --add-port=39999/udp
ip netns exec b python3 "$work/backend.py" > "$work/backend.log" 2>&1 & pids+=($!)
start_relay(){ ip netns exec r "$relaybin" managed "$work/relay.json" >> "$work/relay.out" 2>> "$work/relay.log" & relay=$!; }
start_relay
# The backend has a return route, so keep client addresses (no masquerade).
ip netns exec e env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-e" POC_TRACE_PATHS=1 POC_TEST_DROP_ACTIVE_PATH_FILE="$work/drop" \
  "$bin" endpoint --state-dir "$work/e" --interface rtpe --listen 203.0.113.2:30000 --rotation-ports 30001-30008 \
  --rotation-seconds 2 --initial-rotation-port 30001 --client-address 10.88.0.2,10.88.0.3 \
  --firewall firewalld --firewalld-source 203.0.113.1/32 --no-masquerade \
  --relay-management 203.0.113.1:30200 --relay-server-name relay --relay-ca "$work/ca.pem" \
  --relay-certificate "$work/controller.pem" --relay-private-key "$work/controller.key" \
  --relay-listen-ip 0.0.0.0 --relay-endpoint-ip 203.0.113.2 > "$work/e.out" 2> "$work/e.log" & pids+=($!);epid=$!
ip netns exec c "$bin" client --state-dir "$work/c" --interface rtpc --address 10.88.0.2/24 --outbound-interface cr \
  --entry 192.0.2.254:30000 --bind 192.0.2.1:0 > "$work/c.out" 2> "$work/c.log" & pids+=($!);cpid=$!
ip netns exec d "$bin" client --state-dir "$work/d" --interface rtpd --address 10.88.0.3/24 --outbound-interface dr \
  --entry 198.51.100.254:30000 --bind 198.51.100.1:0 > "$work/d.out" 2> "$work/d.log" & pids+=($!);dpid=$!
for i in $(seq 1 100);do if grep -Eq '"event":"connected"' "$work/c.log" && grep -Eq '"event":"connected"' "$work/d.log";then break;fi;sleep .1;done
apps(){ local n i ok;for n in c d;do ok=0;for i in $(seq 1 6);do if ip netns exec "$n" python3 "$work/app.py" "$n-$1" 2>>"$work/app-retries.log";then ok=1;break;fi;sleep .25;done;[ "$ok" = 1 ] || return 1;done; }
if ! apps initial;then ip netns exec e /usr/sbin/nft list ruleset > "$work/e-nft";ip -n e route show table all > "$work/e-route";exit 1;fi
# Rotation must use reusable numeric ports while current applications keep working.
for i in $(seq 1 80);do apps "rotating-$i";sleep .25;done
grep -Eq '"event":"rotation"' "$work/e.log"
# A real relay firewall reload is reconciled without restarting the endpoint.
fw r --reload
fw r --zone=public --add-port=30200/tcp
fw r --zone=public --add-port=39999/udp
sleep 2
apps reloaded
# Endpoint reload drops runtime state; the service restores its leases and
# TUN zone binding. Only the fixture's own unrelated port is re-added here.
fw e --reload
fw e --zone=public --add-port=39999/udp
sleep 2
[ "$(fw e --get-zone-of-interface=rtpe)" = trusted ]
apps endpoint-reloaded
# Freeze relay management only; packet forwarding continues on committed paths.
fw r --zone=public --add-rich-rule='rule family="ipv4" source address="203.0.113.2/32" port port="30200" protocol="tcp" drop'
sleep 4
apps disconnected
grep -Eq '"event":"degraded"' "$work/e.log"
fw r --zone=public --remove-rich-rule='rule family="ipv4" source address="203.0.113.2/32" port port="30200" protocol="tcp" drop'
sleep 4
apps resumed
# Freeze new rotations during the independent same-port repair injection.
fw r --zone=public --add-rich-rule='rule family="ipv4" source address="203.0.113.2/32" port port="30200" protocol="tcp" drop'
sleep 7
# Existing ping processes survive a tuple blackhole and recover via same-port repair.
pingpids=()
for n in c d;do ip netns exec "$n" ping -c 80 -i .5 -W 1 10.90.0.2 > "$work/$n-ping.log" & pids+=($!);pingpids+=($!);done
sleep 1
touch "$work/drop"
for i in $(seq 1 250);do if grep -Eq '"event":"repaired"' "$work/c.log" "$work/d.log";then break;fi;sleep .1;done
grep -Eq '"event":"repairing"' "$work/c.log" "$work/d.log"
grep -Eq '"event":"repaired"' "$work/c.log" "$work/d.log"
apps repaired
rm "$work/drop"
fw r --zone=public --remove-rich-rule='rule family="ipv4" source address="203.0.113.2/32" port port="30200" protocol="tcp" drop'
sleep 2
kill -KILL "$relay";wait "$relay" 2>/dev/null || true;relay=''
# Independent post-crash generation cleanup, before any listener is recreated.
ip netns exec r "$relaybin" cleanup "$work/relay.json"
ip netns exec r "$relaybin" cleanup "$work/relay.json"
python3 - "$work/r-agent.journal" <<'PYTEST'
import json,sys
assert json.load(open(sys.argv[1]))=={}
PYTEST
start_relay
for i in $(seq 1 40);do if apps restarted > "$work/restart-app.log" 2>&1;then break;fi;sleep .25;done
apps final
python3 - "$work" <<'PYTEST'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1]);events=[json.loads(l) for l in (w/'e.log').read_text().splitlines() if l.startswith('{')]
ports=[e.get('port') for e in events if e.get('event')=='rotation']
committed=set()
for e in events:
 if e.get('event')=='relay_committed':committed.add(e['port'])
 if e.get('event')=='rotation':assert e['port'] in committed,e
assert len(ports)>8 and len(set(ports))<len(ports),ports
# More epochs than numeric pool ensures reuse under real application traffic.
print('rotation events',len(ports))
PYTEST
for p in "${pingpids[@]}";do wait "$p" || true;done
python3 - "$work" <<'PY'
import pathlib,re,sys
w=pathlib.Path(sys.argv[1])
for n in ["c","d"]:
 seq=[int(x) for x in re.findall(r"icmp_seq=(\d+)",(w/(n+"-ping.log")).read_text())]
 assert seq and seq[0]<=3 and seq[-1]>=70,seq
PY
# Stop workers while the relay agent and both firewalld instances are available.
kill -TERM "$cpid" "$dpid" "$epid";wait "$cpid" "$dpid" "$epid"
kill -TERM "$relay";wait "$relay";relay=''
python3 - "$work" <<'PYTEST'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1]);assert json.load(open(w/'relay.journal'))==[]
assert json.load(open(w/'r-agent.journal'))=={}
assert json.load(open(w/'e'/'state.json.firewalld'))=={}
assert not (w/'e'/'state.json.forwarding').exists()
PYTEST
test -z "$(fw e --zone=trusted --list-interfaces)"
test -z "$(fw e --zone=public --list-rich-rules)"
fw r --zone=public --query-port=39999/udp
fw e --zone=public --query-port=39999/udp
cleanup
pids=();relay='' 
for n in c d e;do if ip -n "$n" link show "rtp$n" 2>/dev/null;then exit 1;fi;done
python3 - "$work" <<'PY'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1])
for n in ['c','d','e']:assert json.load(open(w/n/'state.json'))=={'version':1,'leases':[]}
PY
echo "A5_COORDINATED_APPLICATIONS_OK artifacts=$work"
