# Sourced only by the disposable namespace fixture.
fw() { ip netns exec s firewall-cmd "$@"; }
firewall_start() {
  ip netns exec s /usr/sbin/firewalld --nofork --nopid --system-config "$work/firewalld" --log-target console >>"$work/firewalld.log" 2>&1 & firewall=$!
  for i in $(seq 1 100); do if fw --state >/dev/null 2>&1; then return; fi; sleep .1; done
  cat "$work/firewalld.log"; return 1
}
firewall_setup() {
  mkdir -p "$work/firewalld"
  cat > "$work/bus.conf" <<BUS
<busconfig><type>system</type><listen>unix:path=/run/dbus-test</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/><allow user="*"/></policy></busconfig>
BUS
  dbus-daemon --config-file="$work/bus.conf" --fork
  export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus-test
  firewall_start
  fw --zone=public --add-interface=vs
  fw --zone=public --add-port=39999/udp
  fw --permanent --zone=public --add-port=39998/udp
  # Real D-Bus failure replies must not masquerade as readiness.
  if ip netns exec s busctl --address="$DBUS_SYSTEM_BUS_ADDRESS" --timeout=1s call org.fedoraproject.FirewallD1 /org/fedoraproject/FirewallD1 org.fedoraproject.FirewallD1.zone addRichRule ssi public invalid 0 >"$work/failed-add.log" 2>&1; then return 1; fi
  if ip netns exec s busctl --address="$DBUS_SYSTEM_BUS_ADDRESS" --timeout=1s call org.fedoraproject.FirewallD1 /org/fedoraproject/FirewallD1 org.fedoraproject.FirewallD1.zone queryRichRule ss no-such-zone invalid >"$work/failed-query.log" 2>&1; then return 1; fi
  python3 - "$work" <<'PYTEST'
import json, sys, os
work=sys.argv[1]
s=json.load(open(work+'/server.json')); s['admission']={'firewalld':{'agent_socket':work+'/agent.sock'}}
json.dump(s,open(work+'/server.json','w'))
policy={'profile':'server','bind':'192.0.2.2','base':30000,'low':30001,'high':30008,'zone':'public','interface':'vs','sources':['192.0.2.0/24']}
json.dump({'socket':work+'/agent.sock','journal':work+'/agent.journal','allowed_uid':os.getuid(),'policies':[policy,dict(policy,profile='second',base=31000,low=31001,high=31004)]},open(work+'/agent.json','w'))
PYTEST
  agent_bin=$(realpath target/debug/port-admission)
  ip netns exec s "$agent_bin" "$work/agent.json" >"$work/agent.log" 2>&1 & agent=$!
  for i in $(seq 1 50); do if [ -S "$work/agent.sock" ]; then break; fi; sleep .1; done
  ip netns exec s python3 -c 'import socket,time; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(("192.0.2.2",30008));time.sleep(999)' & occupied=$!
  sleep .1
}
firewall_exercise() {
  # Occupied socket is skipped before any firewall mutation.
  kill -0 "$occupied"
  # A second profile shares the agent but has a disjoint namespace and exact
  # generation checks. Repeated and stale requests cannot retire its replacement.
  python3 - "$work/agent.sock" <<'PYTEST'
import socket,json,sys
def call(op,generation):
    lease={'profile':'second','generation':generation,'address':'192.0.2.2:31001','state':'preparing'}
    s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall((json.dumps({'operation':op,'lease':lease})+'\n').encode());r=json.loads(s.makefile().readline());s.close();return r['error']
assert call('prepare','old') is None
assert call('commit','old') is None
assert call('retire','old') is None
assert call('retire','old') is None
assert call('prepare','new') is None
assert call('retire','old') is not None
assert call('commit','new') is None
assert call('retire','new') is None
PYTEST
  # Block every unused candidate with administrator-owned identical rules. Failed
  # prepare must preserve those rules, the epoch, and traffic on the live path.
  python3 - "$work" <<'PYTEST'
import json,sys
w=sys.argv[1]; live={x['address'].split(':')[-1] for x in json.load(open(w+'/server.journal'))['leases']}
open(w+'/conflicts','w').write('\n'.join(str(p) for p in range(30001,30009) if str(p) not in live))
PYTEST
  while read -r port; do fw --zone=public --add-rich-rule="rule family=\"ipv4\" source address=\"192.0.2.0/24\" destination address=\"192.0.2.2\" port port=\"$port\" protocol=\"udp\" accept"; done < <(cat "$work/conflicts"; echo)
  ip netns exec c ping -c 8 -i .5 -W 1 10.88.0.1
  rg -q '"event":"rotation_failed"' "$work/s.log"
  kill "$occupied"; wait "$occupied" 2>/dev/null || true
  occupied=""
  fw --zone=public --query-port=39999/udp
  while read -r port; do fw --zone=public --remove-rich-rule="rule family=\"ipv4\" source address=\"192.0.2.0/24\" destination address=\"192.0.2.2\" port port=\"$port\" protocol=\"udp\" accept"; done < <(cat "$work/conflicts"; echo)
  # A pending/hung control call must not block the packet poller.
  kill -STOP "$agent"
  ip netns exec c ping -c 8 -i .5 -W 1 10.88.0.1
  kill -CONT "$agent"
  sleep 1
  # Restarting the agent retains exact claims; the service restores health.
  kill -TERM "$agent"; wait "$agent" 2>/dev/null || true
  rm "$work/agent.sock"
  ip netns exec s "$agent_bin" "$work/agent.json" >>"$work/agent.log" 2>&1 & agent=$!
  sleep 1
  fw --reload
  sleep 2
  ip netns exec c ping -c 3 -W 2 10.88.0.1
  kill -TERM "$firewall"; wait "$firewall"
  sleep 2
  firewall_start
  sleep 2
  ip netns exec c ping -c 3 -W 2 10.88.0.1
  fw --zone=public --query-port=39998/udp
  fw --permanent --zone=public --query-port=39998/udp
  fw --zone=public --add-port=39999/udp
}
firewall_assert_clean() {
  test -z "$(fw --zone=public --list-rich-rules)"
  fw --zone=public --query-port=39999/udp
  fw --permanent --zone=public --query-port=39998/udp
  python3 - "$work/agent.journal" <<'PYTEST'
import json,sys
assert json.load(open(sys.argv[1])) == {}
PYTEST
}
