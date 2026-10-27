#!/bin/bash
set -euo pipefail
# Run: cargo build -p rtp-service; unshare -Urnm bash rtp-service/tests/linux-lifecycle.sh
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
work=$(mktemp -d /tmp/rtp-a1-XXXX)
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export CPXY_RTP_KEEPALIVE_SECONDS=5
server_env=(env CPXY_RTP_STATE_DIR="$work/server" CPXY_RTP_INTERFACE=rtps CPXY_RTP_LISTEN=192.0.2.2:30000
  CPXY_RTP_UDP_PORTS=30001-30008 CPXY_RTP_FIREWALL=none)
client_env=(env CPXY_RTP_STATE_DIR="$work/client" CPXY_RTP_INTERFACE=rtpc
  CPXY_RTP_ENTRY=192.0.2.2:30000 CPXY_RTP_BIND=192.0.2.1:0 CPXY_RTP_DYNAMIC_ADDRESS=true)
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
ip netns exec s "${server_env[@]}" "$bin" endpoint >"$work/s.out" 2>"$work/s.log" & server=$!
ip netns exec c "${client_env[@]}" "$bin" client >"$work/c.out" 2>"$work/c.log" & client=$!
ping_pid=""
cleanup() {
  for pid in "$server" "$client" "$ping_pid"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
  for pid in "$server" "$client" "$ping_pid"; do
    if [ -n "$pid" ]; then wait "$pid" 2>/dev/null || true; fi
  done
}
trap 'status=$?; cleanup; rm -rf "$work"; exit "$status"' EXIT
# Debug builds derive the Argon2 key slowly on CI; allow a generous connect.
connected(){
  for i in $(seq 1 300); do
    if [ "$(grep -Ec '"event":"connected"' "$work/c.log")" -ge "$1" ]; then return; fi
    sleep .1
  done
  echo "client did not connect" >&2; tail -n 40 "$work/c.log" "$work/s.log" "$work/c.out" "$work/s.out" >&2 || true; exit 1
}
connected 1
ip netns exec c ping -c 4 -W 2 10.88.0.1
if [ "${RTP_TEST_REPAIR:-0}" = 1 ]; then
  # The same ping process stays alive while the active tuple is blackholed.
  ip netns exec c ping -c 100 -i .5 -W 1 10.88.0.1 > "$work/repair-ping.log" & ping_pid=$!
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
  ip netns exec c ping -c 4 -W 2 10.88.0.1
  ip netns exec c nft delete table inet rtp_repair
fi
# SIGHUP is a graceful stop: the TUN is removed.
kill -HUP "$client"
wait "$client"
if ip -n c link show rtpc 2>/dev/null; then exit 1; fi
expected=$(( $(grep -c '"event":"connected"' "$work/c.log") + 1 ))
ip netns exec c "${client_env[@]}" "$bin" client >>"$work/c.out" 2>>"$work/c.log" & client=$!
connected "$expected"
ip netns exec c ping -c 4 -W 2 10.88.0.1
kill -TERM "$client" "$server"
wait "$client"
wait "$server"
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
echo "HTTP_NAMESPACE_OK cleanup=verified"
