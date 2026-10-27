#!/bin/bash
set -euo pipefail
# Run: cargo build -p rtp-service; unshare -Urnm bash rtp-service/tests/linux-firewalld.sh
# All interfaces, routing and process operations stay inside these namespaces.
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ]; then
  echo 'Run this fixture inside unshare -Urnm' >&2
  exit 1
fi
mount --make-rprivate /
mount -t tmpfs tmpfs /run
ip link set lo up
ip netns add c
ip netns add s
ip link add vc type veth peer name vs
ip link set vc netns c
ip link set vs netns s
ip -n c addr add 192.0.2.1/24 dev vc
ip -n s addr add 192.0.2.2/24 dev vs
ip -n c link set vc up
ip -n s link set vs up
ip -n c link set lo up
ip -n s link set lo up
# Backend deliberately has no route back to the tunnel pool.
ip netns add b
ip link add sb type veth peer name bs
ip link set sb netns s
ip link set bs netns b
ip -n s addr add 10.90.0.1/24 dev sb
ip -n b addr add 10.90.0.2/24 dev bs
ip -n s link set sb up
ip -n b link set bs up
ip -n b link set lo up
ip netns exec s python3 -c 'open("/proc/sys/net/ipv4/ip_forward","w").write("1")'
work=$(mktemp -d /tmp/rtp-a2-XXXX)
cat > "$work/backend.py" <<'PY'
import socket,threading
def tcp():
 s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('10.90.0.2',8080));s.listen()
 while True:
  c,a=s.accept();d=c.recv(100);c.sendall(b'tcp:'+d);c.close()
threading.Thread(target=tcp,daemon=True).start()
u=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);u.bind(('10.90.0.2',8080))
while True:
 d,a=u.recvfrom(100);u.sendto(b'udp:'+d,a)
PY
cat > "$work/app.py" <<'PY'
import socket
for kind,prefix in [(socket.SOCK_STREAM,b'tcp:'),(socket.SOCK_DGRAM,b'udp:')]:
 s=socket.socket(socket.AF_INET,kind);s.settimeout(2);s.connect(('10.90.0.2',8080));s.send(b'application');assert s.recv(100)==prefix+b'application';s.close()
PY
ip netns exec b python3 "$work/backend.py" > "$work/backend.log" 2>&1 & backend=$!
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export CPXY_RTP_KEEPALIVE_SECONDS=5 CPXY_RTP_SERVICE_CIDRS=10.90.0.0/24
# `auto` must discover firewalld on the (private) system bus.
server_env=(env CPXY_RTP_STATE_DIR="$work/server" CPXY_RTP_INTERFACE=rtps CPXY_RTP_LISTEN=192.0.2.2:30000
  CPXY_RTP_UDP_PORTS=30001-30008
  CPXY_RTP_FIREWALL=auto CPXY_RTP_FIREWALLD_SOURCES=192.0.2.0/24 CPXY_RTP_ROUTE_TABLE=18801)
client_env=(env CPXY_RTP_STATE_DIR="$work/client" CPXY_RTP_INTERFACE=rtpc CPXY_RTP_OUTBOUND_INTERFACE=vc
  CPXY_RTP_ENTRY=192.0.2.2:30000 CPXY_RTP_BIND=192.0.2.1:0)
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
source rtp-service/tests/firewalld-helpers.sh
firewall_setup
ip netns exec s "${server_env[@]}" "$bin" endpoint >"$work/s.out" 2>"$work/s.log" & server=$!
ip netns exec c "${client_env[@]}" "$bin" client >"$work/c.out" 2>"$work/c.log" & client=$!
ping_pid=""
cleanup() {
  for pid in "$server" "$client" "$ping_pid" "${backend:-}" "${firewall:-}" "${occupied:-}"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
  for pid in "$server" "$client" "$ping_pid" "${backend:-}" "${firewall:-}" "${occupied:-}"; do
    if [ -n "$pid" ]; then wait "$pid" 2>/dev/null || true; fi
  done
}
trap 'status=$?; cleanup; rm -rf "$work"; exit "$status"' EXIT
for i in $(seq 1 300); do
  if grep -Eq '"event":"connected"' "$work/c.log"; then break; fi
  sleep .1
done
ip netns exec c ping -c 4 -W 2 10.90.0.2
grep -Eq '"event":"admission","interface":"vs","mode":"firewalld","zone":"public"' "$work/s.log"
if [ "${RTP_TEST_REPAIR:-0}" = 1 ]; then
  # The same ping process stays alive while the active tuple is blackholed.
  ip netns exec c ping -c 100 -i .5 -W 1 10.90.0.2 > "$work/repair-ping.log" & ping_pid=$!
  sleep 1.2
  # Blackhole only the current UDP allocation; HTTP remains available.
  port=$(python3 - "$work/server/state.json" <<'PYPORT'
import json,sys
print(json.load(open(sys.argv[1]))['leases'][0]['address'].rsplit(':',1)[1])
PYPORT
  )
  ip netns exec c nft add table inet rtp_repair
  ip netns exec c nft 'add chain inet rtp_repair output { type filter hook output priority -10; policy accept; }'
  ip netns exec c nft add rule inet rtp_repair output udp dport "$port" drop
  for i in $(seq 1 700); do
    if test "$(grep -Ec '"event":"connected"' "$work/c.log")" -ge 2; then break; fi
    sleep .1
  done
  grep -Eq '"event":"repairing"' "$work/c.log"
  test "$(grep -Ec '"event":"connected"' "$work/c.log")" -ge 2
  wait "$ping_pid" || true
  ping_pid=""
  python3 - "$work/repair-ping.log" <<'PYTEST'
import re, sys
seqs = [int(x) for x in re.findall(r'icmp_seq=(\d+)', open(sys.argv[1]).read())]
assert seqs and seqs[0] <= 3, seqs
assert any(b > a + 1 for a, b in zip(seqs, seqs[1:])), seqs
assert seqs[-1] >= 90, seqs
PYTEST
  ip netns exec c ping -c 4 -W 2 10.90.0.2
  ip netns exec c nft delete table inet rtp_repair
fi
firewall_exercise
kill -TERM "$client"
wait "$client"
if ip -n c link show rtpc 2>/dev/null; then exit 1; fi
ip netns exec c "${client_env[@]}" "$bin" client >>"$work/c.out" 2>>"$work/c.log" & client=$!
sleep 2
ip netns exec c ping -c 4 -W 2 10.90.0.2
kill -TERM "$client"
wait "$client"
# SIGKILL leaves journaled ports and forwarding; independent cleanup removes them.
kill -KILL "$server"
wait "$server" 2>/dev/null || true
forwarding_present
ip netns exec s "${server_env[@]}" "$bin" endpoint cleanup
ip netns exec s "${server_env[@]}" "$bin" endpoint cleanup
firewall_assert_clean
# Graceful stop releases the same state without explicit cleanup.
ip netns exec s "${server_env[@]}" "$bin" endpoint >>"$work/s.out" 2>>"$work/s.log" & server=$!
ip netns exec c "${client_env[@]}" "$bin" client >>"$work/c.out" 2>>"$work/c.log" & client=$!
backend_app
kill -TERM "$client" "$server"
wait "$client"
wait "$server"
firewall_assert_clean
# An explicit firewalld request fails closed when firewalld is unavailable.
if ip netns exec s env DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/no-bus "${server_env[@]}" CPXY_RTP_FIREWALL=firewalld "$bin" endpoint >"$work/absent.out" 2>"$work/absent.log"; then exit 1; fi
grep -q 'firewalld requested but not answering' "$work/absent.log"
if ip -n c link show rtpc 2>/dev/null || ip -n s link show rtps 2>/dev/null; then exit 1; fi
python3 - "$work/server/state.json" "$work/client/state.json" <<'PYTEST'
import json, sys
for path in sys.argv[1:]:
    journal = json.load(open(path))
    assert journal == {"version": 1, "leases": []}, journal
PYTEST
# Bound listener descriptors must also be released, including allocation ports.
ip netns exec s python3 - <<'PYTEST'
import socket
sockets = []
for port in range(30000, 30009):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("192.0.2.2", port))
    sockets.append(sock)
PYTEST
cat "$work/c.log"
echo "HTTP_FIREWALL_NAMESPACE_OK cleanup=verified"
