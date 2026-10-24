#!/bin/sh
# Run on Linux as ordinary user. All network changes live in disposable netns.
# Usage: sh local-integration.sh ABSOLUTE_CRATE_DIR PRIVATE_KEY_FILE OUTPUT_DIR
set -eu
poc_crate=$1
poc_key=$2
poc_out=$3
mkdir -p "$poc_out"
chmod 700 "$poc_out"
export poc_crate poc_key poc_out
timeout 180 unshare -Urn sh -eu <<'NS'
ip link set lo up
poc_server=
poc_iperf=
poc_proxy=
cleanup() {
  for poc_pid in "$poc_proxy" "$poc_iperf" "$poc_server"; do
    if [ -n "$poc_pid" ]; then kill -TERM "$poc_pid" 2>/dev/null || true; wait "$poc_pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT INT TERM
"$poc_crate/target/release/rtp-l3-poc" server "$poc_key" rtptest0 192.0.2.1/24 127.0.0.1:45983 2>"$poc_out/server.log" &
poc_server=$!
poc_wait=0
until ip -o -4 addr show dev rtptest0 2>/dev/null | grep -q '192.0.2.1/24'; do
  kill -0 "$poc_server" 2>/dev/null || exit 1
  poc_wait=$((poc_wait+1)); [ "$poc_wait" -lt 100 ] || exit 1
  sleep 0.1
done
iperf3 -s -B 192.0.2.1 -p 45201 >"$poc_out/iperf-server.log" 2>&1 &
poc_iperf=$!
cat >"$poc_out/clean.sh" <<'RUN'
set -eu
ping -c 10 -i 0.05 192.0.2.1
timeout 45 iperf3 -c 192.0.2.1 -p 45201 -n 100M -J >"$poc_out/clean-up.json"
timeout 45 iperf3 -c 192.0.2.1 -p 45201 -n 100M -R -J >"$poc_out/clean-down.json"
RUN
python3 "$poc_crate/scripts/client-netns.py" "$poc_crate/target/release/rtp-l3-poc" "$poc_key" 127.0.0.1:45983 rtptest1 192.0.2.2/24 "$poc_out/clean.sh" >"$poc_out/clean.txt" 2>"$poc_out/client-clean.log"
python3 "$poc_crate/scripts/fault-proxy.py" 45984 45983 >"$poc_out/faults.txt" 2>&1 &
poc_proxy=$!
sleep 0.2
cat >"$poc_out/fault.sh" <<'RUN'
set -eu
ping -c 20 -i 0.05 192.0.2.1
timeout 45 iperf3 -c 192.0.2.1 -p 45201 -t 4 -J >"$poc_out/fault-up.json"
timeout 45 iperf3 -c 192.0.2.1 -p 45201 -t 4 -R -J >"$poc_out/fault-down.json"
timeout 45 iperf3 -c 192.0.2.1 -p 45201 -t 3 -u -b 5M -l 1000 -J >"$poc_out/fault-udp.json"
RUN
python3 "$poc_crate/scripts/client-netns.py" "$poc_crate/target/release/rtp-l3-poc" "$poc_key" 127.0.0.1:45984 rtptest1 192.0.2.2/24 "$poc_out/fault.sh" >"$poc_out/fault.txt" 2>"$poc_out/client-fault.log"
python3 - "$poc_out" <<'CHECK'
import json, pathlib, sys
out=pathlib.Path(sys.argv[1])
for name in ('clean-up','clean-down','fault-up','fault-down','fault-udp'):
    result=json.loads((out/(name+'.json')).read_text())
    assert 'error' not in result,(name,result.get('error'))
    end=result['end']
    received=end.get('sum_received',end.get('sum',{}))
    assert received.get('bytes',0)>0,(name,end)
    if name.startswith('clean-'):
        assert received['bytes']>=100*1024*1024,(name,received)
assert 'dropped control 2' in (out/'faults.txt').read_text()
assert 'dropped control 4' in (out/'faults.txt').read_text()
CHECK
echo 'PASS local integration: 100MiB each direction across rollover, session replacement, lost CHALLENGE/READY, deterministic UDP faults'
NS
