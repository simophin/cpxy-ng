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
  bus=$(dbus-daemon --config-file="$work/bus.conf" --fork --print-pid=1)
  export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus-test
  firewall_start
  fw --zone=public --add-interface=vs
  fw --zone=public --add-port=30000/tcp
  fw --permanent --zone=public --add-port=30000/tcp
  fw --zone=public --add-port=39999/udp
  fw --permanent --zone=public --add-port=39998/udp
  # Real D-Bus failure replies must not masquerade as readiness.
  if ip netns exec s busctl --address="$DBUS_SYSTEM_BUS_ADDRESS" --timeout=1s call org.fedoraproject.FirewallD1 /org/fedoraproject/FirewallD1 org.fedoraproject.FirewallD1.zone addRichRule ssi public invalid 0 >"$work/failed-add.log" 2>&1; then return 1; fi
  if ip netns exec s busctl --address="$DBUS_SYSTEM_BUS_ADDRESS" --timeout=1s call org.fedoraproject.FirewallD1 /org/fedoraproject/FirewallD1 org.fedoraproject.FirewallD1.zone queryRichRule ss no-such-zone invalid >"$work/failed-query.log" 2>&1; then return 1; fi
  ip netns exec s python3 -c 'import socket,time; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(("192.0.2.2",30008));time.sleep(999)' & occupied=$!
  sleep .1
}
# TCP/UDP to the backend, which has no route back to the tunnel pool: only the
# service's TUN zone binding plus scoped masquerade make this work.
backend_app() {
  for i in $(seq 1 30); do
    if ip netns exec c python3 "$work/app.py" 2>/dev/null; then return; fi
    sleep .5
  done
  ip netns exec c python3 "$work/app.py"
}
masq='rule family="ipv4" source address="10.88.0.0/24" destination address="10.90.0.0/24" masquerade'
forwarding_present() {
  [ "$(fw --get-zone-of-interface=rtps)" = trusted ]
  fw --zone=public --query-rich-rule="$masq"
}
firewall_exercise() {
  # Occupied sockets must not be disturbed by allocation.
  kill -0 "$occupied"
  forwarding_present
  backend_app
  # Add administrator-owned rules on other fixture-range ports. These remain
  # unrelated rules in OS-assigned mode; active traffic and ownership survive.
  python3 - "$work" <<'PYTEST'
import json,sys
w=sys.argv[1]; live={x['address'].split(':')[-1] for x in json.load(open(w+'/server/state.json'))['leases']}
open(w+'/conflicts','w').write('\n'.join(str(p) for p in range(30001,30009) if str(p) not in live))
PYTEST
  while read -r port; do fw --zone=public --add-rich-rule="rule family=\"ipv4\" source address=\"192.0.2.0/24\" destination address=\"192.0.2.2\" port port=\"$port\" protocol=\"udp\" accept"; done < <(cat "$work/conflicts"; echo)
  ip netns exec c ping -c 8 -i .5 -W 1 10.90.0.2
  # Five-minute rotation/failure semantics are checked by the Rust transport tests.
  kill "$occupied"; wait "$occupied" 2>/dev/null || true
  occupied=""
  fw --zone=public --query-port=39999/udp
  while read -r port; do fw --zone=public --remove-rich-rule="rule family=\"ipv4\" source address=\"192.0.2.0/24\" destination address=\"192.0.2.2\" port port=\"$port\" protocol=\"udp\" accept"; done < <(cat "$work/conflicts"; echo)
  # A hung firewalld (pending D-Bus calls) must not block the packet poller.
  kill -STOP "$firewall"
  ip netns exec c ping -c 8 -i .5 -W 1 10.90.0.2
  kill -CONT "$firewall"
  sleep 1
  # Reload drops runtime state; reconciliation restores ports and forwarding.
  fw --reload
  sleep 3
  ip netns exec c ping -c 3 -W 2 10.90.0.2
  forwarding_present
  backend_app
  kill -TERM "$firewall"; wait "$firewall"
  sleep 2
  firewall_start
  fw --zone=public --add-interface=vs
  sleep 3
  ip netns exec c ping -c 3 -W 2 10.90.0.2
  forwarding_present
  backend_app
  fw --zone=public --query-port=39998/udp
  fw --permanent --zone=public --query-port=39998/udp
  fw --zone=public --add-port=39999/udp
}
firewall_assert_clean() {
  test -z "$(fw --zone=public --list-rich-rules)"
  test -z "$(fw --zone=trusted --list-interfaces)"
  fw --zone=public --query-port=39999/udp
  fw --permanent --zone=public --query-port=39998/udp
  python3 - "$work/server/state.json.firewalld" <<'PYTEST'
import json,sys
assert json.load(open(sys.argv[1])) == {}
PYTEST
  test ! -e "$work/server/state.json.forwarding"
}
