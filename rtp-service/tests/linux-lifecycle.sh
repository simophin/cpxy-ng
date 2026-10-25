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
head -c 32 /dev/urandom > "$work/key"
chmod 600 "$work/key"
cat > "$work/server.json" <<JSON
{"profile":"server","key_file":"$work/key","journal":"$work/server.journal","interface":"rtps","address":"10.88.0.1/24","transport":{"role":"endpoint","listen":"192.0.2.2:30000","rotation_low":30001,"rotation_high":30008,"rotation_seconds":2},"admission":"administrator_managed","keepalive_seconds":5}
JSON
cat > "$work/client.json" <<JSON
{"profile":"client","key_file":"$work/key","journal":"$work/client.journal","interface":"rtpc","outbound_interface":"vc","address":"10.88.0.2/24","transport":{"role":"client","entry":"192.0.2.2:30000","bind":"192.0.2.1:0"},"admission":"administrator_managed","keepalive_seconds":5}
JSON
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
if [ "${RTP_TEST_REPAIR:-0}" = 1 ]; then
  sed -i 's/"rotation_seconds":2/"rotation_seconds":0/' "$work/server.json"
fi
ip netns exec s env POC_TEST_DROP_ACTIVE_PATH_FILE="$work/drop" "$bin" run "$work/server.json" >"$work/s.out" 2>"$work/s.log" & server=$!
ip netns exec c "$bin" run "$work/client.json" >"$work/c.out" 2>"$work/c.log" & client=$!
ping_pid=""
cleanup() {
  for pid in "$server" "$client" "$ping_pid"; do
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  done
  for pid in "$server" "$client" "$ping_pid"; do
    if [ -n "$pid" ]; then wait "$pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT
for i in $(seq 1 80); do
  if rg -q '"event":"connected"' "$work/c.log"; then break; fi
  sleep .1
done
ip netns exec c ping -c 4 -W 2 10.88.0.1
if [ "${RTP_TEST_REPAIR:-0}" = 1 ]; then
  # The same ping process stays alive while the active tuple is blackholed.
  ip netns exec c ping -c 25 -i .5 -W 1 10.88.0.1 > "$work/repair-ping.log" & ping_pid=$!
  sleep 1.2
  touch "$work/drop"
  for i in $(seq 1 200); do
    if rg -q '"event":"repaired"' "$work/c.log"; then break; fi
    sleep .1
  done
  rg -q '"event":"repairing"' "$work/c.log"
  rg -q '"event":"repaired"' "$work/c.log"
  wait "$ping_pid" || true
  ping_pid=""
  python3 - "$work/repair-ping.log" <<'PYTEST'
import re, sys
seqs = [int(x) for x in re.findall(r'icmp_seq=(\d+)', open(sys.argv[1]).read())]
assert seqs and seqs[0] <= 3, seqs
assert any(b > a + 1 for a, b in zip(seqs, seqs[1:])), seqs
assert seqs[-1] >= 23, seqs
PYTEST
  ip netns exec c ping -c 4 -W 2 10.88.0.1
  rm "$work/drop"
fi
kill -HUP "$client"
sleep 2
ip netns exec c ping -c 4 -W 2 10.88.0.1
kill -TERM "$client"
wait "$client"
if ip -n c link show rtpc 2>/dev/null; then exit 1; fi
ip netns exec c "$bin" run "$work/client.json" >>"$work/c.out" 2>>"$work/c.log" & client=$!
sleep 2
ip netns exec c ping -c 4 -W 2 10.88.0.1
kill -TERM "$client" "$server"
wait "$client"
wait "$server"
if ip -n c link show rtpc 2>/dev/null || ip -n s link show rtps 2>/dev/null; then exit 1; fi
python3 - "$work/server.journal" "$work/client.journal" <<'PYTEST'
import json, sys
for path in sys.argv[1:]:
    journal = json.load(open(path))
    assert journal == {"version": 1, "leases": []}, journal
PYTEST
# Bound listener descriptors must also be released, including rotation ports.
ip netns exec s python3 - <<'PYTEST'
import socket
sockets = []
for port in range(30000, 30009):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("192.0.2.2", port))
    sockets.append(sock)
PYTEST
cat "$work/c.log"
echo "A1_NAMESPACE_OK artifacts=$work"
