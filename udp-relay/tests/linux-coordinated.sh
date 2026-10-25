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
head -c 32 /dev/urandom > "$work/key";chmod 600 "$work/key"
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
w=Path(sys.argv[1]);network={'tunnel_pool':'10.88.0.0/24','client_addresses':['10.88.0.2','10.88.0.3'],'service_cidrs':['10.90.0.0/24'],'table':18800,'rule_priority':1000}
for name,addr,transport in [('e','10.88.0.1',{'role':'endpoint','listen':'203.0.113.2:30000','rotation_low':30001,'rotation_high':30008,'rotation_seconds':2}),('c','10.88.0.2',{'role':'client','entry':'192.0.2.254:30000','bind':'192.0.2.1:0'}),('d','10.88.0.3',{'role':'client','entry':'198.51.100.254:30000','bind':'198.51.100.1:0'})]:
 p={'profile':name,'key_file':str(w/'key'),'journal':str(w/(name+'.journal')),'interface':'rtp'+name,'address':addr+'/24','transport':transport,'admission':'administrator_managed','keepalive_seconds':5,'network':network}
 if name=='e':
  p['initial_rotation_port']=30001
  p['admission']={'firewalld':{'agent_socket':str(w/'e-agent.sock')}}
  p['relay']={'management':{'address':'203.0.113.1:30200','server_name':'relay','tls':{'ca':str(w/'ca.pem'),'certificate':str(w/'controller.pem'),'private_key':str(w/'controller.key')},'owner':'e'},'listen_ip':'0.0.0.0','endpoint_ip':'203.0.113.2'}
 else:p['outbound_interface']=name+'r'
 json.dump(p,open(w/(name+'.json'),'w'))
c={'mappings':[{'id':str(p),'generation':'fixed-1','listen':'0.0.0.0:'+str(p),'upstream':'203.0.113.2:'+str(p),'max_flows':128} for p in [30000,30001]],'max_flows':256,'max_source_flows':64,'max_payload':2048,'socket_buffer':32768,'idle_seconds':180,'packets_per_second':100000,'bytes_per_second':100000000,'mapping_packets_per_second':100000,'mapping_bytes_per_second':100000000,'source_packets_per_second':100000,'source_bytes_per_second':100000000}
c['mappings']=[]
m={'relay':c,'management':'203.0.113.1:30200','tls':{'ca':str(w/'ca.pem'),'certificate':str(w/'relay.pem'),'private_key':str(w/'relay.key')},'authority':{'owner':'e','listen_ip':'0.0.0.0','upstream_ip':'203.0.113.2','ports':list(range(30000,30009)),'max_flows':128},'journal':str(w/'relay.journal'),'agent_socket':str(w/'r-agent.sock'),'staged_seconds':15,'outage_seconds':120}
json.dump(m,open(w/'relay.json','w'))
import os
for n,bind,interface,sources in [('r','0.0.0.0','rc',['192.0.2.0/24','198.51.100.0/24']),('e','203.0.113.2','er',['203.0.113.1/32'])]:
 policy={'profile':'e','bind':bind,'base':30000,'low':30001,'high':30008,'zone':'public','interface':interface,'sources':sources}
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
 ip netns exec "$n" env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-$n" "$agentbin" "$work/$n-agent.json" >"$work/$n-agent.log" 2>&1 & pids+=($!)
done
fw(){ local n=$1;shift;ip netns exec "$n" env DBUS_SYSTEM_BUS_ADDRESS="unix:path=/run/dbus-$n" firewall-cmd "$@"; }
fw r --zone=public --add-port=30200/tcp
fw r --zone=public --add-port=39999/udp
fw e --zone=public --add-port=39999/udp
fw e --zone=public --add-interface=eb
fw e --zone=public --add-interface=rtpe
fw e --zone=public --add-forward
# Explicit scoped forwarding for the disposable backend topology.
fw e --zone=public --add-rich-rule='rule family="ipv4" source address="10.88.0.0/24" destination address="10.90.0.0/24" accept'
fw e --zone=public --add-rich-rule='rule family="ipv4" source address="10.90.0.0/24" destination address="10.88.0.0/24" accept'
ip netns exec b python3 "$work/backend.py" > "$work/backend.log" 2>&1 & pids+=($!)
start_relay(){ ip netns exec r "$relaybin" managed "$work/relay.json" >> "$work/relay.out" 2>> "$work/relay.log" & relay=$!; }
start_relay
ip netns exec e env POC_TRACE_PATHS=1 POC_TEST_DROP_ACTIVE_PATH_FILE="$work/drop" "$bin" run "$work/e.json" > "$work/e.out" 2> "$work/e.log" & pids+=($!)
for n in c d;do ip netns exec "$n" "$bin" run "$work/$n.json" > "$work/$n.out" 2> "$work/$n.log" & pids+=($!);done
for i in $(seq 1 100);do if rg -q '"event":"connected"' "$work/c.log" && rg -q '"event":"connected"' "$work/d.log";then break;fi;sleep .1;done
apps(){ local n i ok;for n in c d;do ok=0;for i in $(seq 1 6);do if ip netns exec "$n" python3 "$work/app.py" "$n-$1" 2>>"$work/app-retries.log";then ok=1;break;fi;sleep .25;done;[ "$ok" = 1 ] || return 1;done; }
if ! apps initial;then ip netns exec e /usr/sbin/nft list ruleset > "$work/e-nft";ip -n e route show table all > "$work/e-route";exit 1;fi
# Rotation must use reusable numeric ports while current applications keep working.
for i in $(seq 1 80);do apps "rotating-$i";sleep .25;done
rg -q '"event":"rotation"' "$work/e.log"
# A real relay firewall reload is reconciled without restarting the endpoint.
fw r --reload
fw r --zone=public --add-port=30200/tcp
fw r --zone=public --add-port=39999/udp
sleep 2
apps reloaded
# Endpoint reload also removes fixture-owned runtime forwarding; restore that
# explicit deployment policy independently, while the agent restores its leases.
fw e --reload
fw e --zone=public --add-interface=eb
fw e --zone=public --add-interface=rtpe
fw e --zone=public --add-forward
fw e --zone=public --add-port=39999/udp
fw e --zone=public --add-rich-rule='rule family="ipv4" source address="10.88.0.0/24" destination address="10.90.0.0/24" accept'
fw e --zone=public --add-rich-rule='rule family="ipv4" source address="10.90.0.0/24" destination address="10.88.0.0/24" accept'
sleep 2
apps endpoint-reloaded
# Freeze relay management only; packet forwarding continues on committed paths.
fw r --zone=public --add-rich-rule='rule family="ipv4" source address="203.0.113.2/32" port port="30200" protocol="tcp" drop'
sleep 4
apps disconnected
rg -q '"event":"degraded"' "$work/e.log"
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
for i in $(seq 1 250);do if rg -q '"event":"repaired"' "$work/c.log" "$work/d.log";then break;fi;sleep .1;done
rg -q '"event":"repairing"' "$work/c.log" "$work/d.log"
rg -q '"event":"repaired"' "$work/c.log" "$work/d.log"
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
# Stop workers while both local agents and firewalld are still available.
for n in c d e;do pkill -TERM -f "^$bin run $work/$n.json$" || true;done
sleep 2
kill -TERM "$relay";wait "$relay";relay=''
python3 - "$work" <<'PYTEST'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1]);assert json.load(open(w/'relay.journal'))==[]
for n in ['r','e']:assert json.load(open(w/(n+'-agent.journal')))=={}
PYTEST
fw r --zone=public --query-port=39999/udp
fw e --zone=public --query-port=39999/udp
cleanup
pids=();relay='' 
for n in c d e;do if ip -n "$n" link show "rtp$n" 2>/dev/null;then exit 1;fi;done
python3 - "$work" <<'PY'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1])
for n in ['c','d','e']:assert json.load(open(w/(n+'.journal')))=={'version':1,'leases':[]}
PY
echo "A5_COORDINATED_APPLICATIONS_OK artifacts=$work"
