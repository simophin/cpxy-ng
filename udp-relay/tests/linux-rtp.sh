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
work=$(mktemp -d /tmp/rtp-a4-XXXX)
head -c 32 /dev/urandom > "$work/key";chmod 600 "$work/key"
python3 - "$work" <<'PY'
import json,sys
from pathlib import Path
w=Path(sys.argv[1]);network={'tunnel_pool':'10.88.0.0/24','client_addresses':['10.88.0.2','10.88.0.3'],'service_cidrs':['10.90.0.0/24'],'table':18800,'rule_priority':1000}
for name,addr,transport in [('e','10.88.0.1',{'role':'endpoint','listen':'203.0.113.2:30000','rotation_low':30001,'rotation_high':30008,'rotation_seconds':0}),('c','10.88.0.2',{'role':'client','entry':'192.0.2.254:30000','bind':'192.0.2.1:0'}),('d','10.88.0.3',{'role':'client','entry':'198.51.100.254:30000','bind':'198.51.100.1:0'})]:
 p={'profile':name,'key_file':str(w/'key'),'journal':str(w/(name+'.journal')),'interface':'rtp'+name,'address':addr+'/24','transport':transport,'admission':'administrator_managed','keepalive_seconds':5,'network':network}
 if name=='e':p['initial_rotation_port']=30001
 else:p['outbound_interface']=name+'r'
 json.dump(p,open(w/(name+'.json'),'w'))
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
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
relaybin=$(realpath "${UDP_RELAY_BIN:-target/debug/cpxy-udp-relay}")
pids=();relay=''
cleanup(){ for p in "${pids[@]}" "$relay";do [ -z "$p" ] || kill "$p" 2>/dev/null || true;done;for p in "${pids[@]}" "$relay";do [ -z "$p" ] || wait "$p" 2>/dev/null || true;done; }
trap cleanup EXIT
ip netns exec b python3 "$work/backend.py" > "$work/backend.log" 2>&1 & pids+=($!)
start_relay(){ ip netns exec r "$relaybin" run "$work/relay.json" >> "$work/relay.out" 2>> "$work/relay.log" & relay=$!; }
start_relay
ip netns exec e env POC_TEST_DROP_ACTIVE_PATH_FILE="$work/drop" "$bin" run "$work/e.json" > "$work/e.out" 2> "$work/e.log" & pids+=($!)
for n in c d;do ip netns exec "$n" "$bin" run "$work/$n.json" > "$work/$n.out" 2> "$work/$n.log" & pids+=($!);done
for i in $(seq 1 100);do if rg -q '"event":"connected"' "$work/c.log" && rg -q '"event":"connected"' "$work/d.log";then break;fi;sleep .1;done
apps(){ for n in c d;do ip netns exec "$n" python3 "$work/app.py" "$n-$1";done; }
apps initial
# Exactly two listeners; never expose the endpoint's remaining allocation pool.
ip netns exec r python3 - <<'PY'
import socket
s=[]
for p in range(30002,30009):
 x=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);x.bind(('0.0.0.0',p));s.append(x)
PY
# Existing ping processes survive a tuple blackhole and recover via same-port repair.
pingpids=()
for n in c d;do ip netns exec "$n" ping -c 35 -i .5 -W 1 10.90.0.2 > "$work/$n-ping.log" & pids+=($!);pingpids+=($!);done
sleep 1
touch "$work/drop"
for i in $(seq 1 250);do if rg -q '"event":"repaired"' "$work/c.log" "$work/d.log";then break;fi;sleep .1;done
rg -q '"event":"repairing"' "$work/c.log" "$work/d.log"
rg -q '"event":"repaired"' "$work/c.log" "$work/d.log"
apps repaired
rm "$work/drop"
kill -TERM "$relay";wait "$relay";relay=''
start_relay
for i in $(seq 1 40);do if apps restarted > "$work/restart-app.log" 2>&1;then break;fi;sleep .25;done
apps final
# Validate reconnect did not rotate: current advertised path remains selected nonbase.
python3 - "$work" <<'PY'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1]);events=[json.loads(l) for l in (w/'e.log').read_text().splitlines() if l.startswith('{')]
assert not any(e.get('event')=='rotated' for e in events),events
for name in ['c','d']:
 events=[json.loads(l) for l in (w/(name+'.log')).read_text().splitlines() if l.startswith('{')]
 assert any(e.get('event')=='connected' for e in events),events
relay=[json.loads(l) for l in (w/'relay.log').read_text().splitlines() if l.startswith('{')]
stopped=[e for e in relay if e.get('event')=='stopped']
assert stopped and all(m['counters']['sent']>0 for m in stopped[0]['inventory']['mappings']),stopped
PY
for p in "${pingpids[@]}";do wait "$p" || true;done
python3 - "$work" <<'PY'
import pathlib,re,sys
w=pathlib.Path(sys.argv[1])
for n in ["c","d"]:
 seq=[int(x) for x in re.findall(r"icmp_seq=(\d+)",(w/(n+"-ping.log")).read_text())]
 assert seq and seq[0]<=3 and seq[-1]>=33,seq
PY
cleanup
pids=();relay=''
for n in c d e;do if ip -n "$n" link show "rtp$n" 2>/dev/null;then exit 1;fi;done
python3 - "$work" <<'PY'
import json,sys,pathlib
w=pathlib.Path(sys.argv[1])
for n in ['c','d','e']:assert json.load(open(w/(n+'.journal')))=={'version':1,'leases':[]}
PY
echo "A4_RELAY_APPLICATIONS_OK artifacts=$work"
