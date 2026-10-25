#!/bin/sh
# Prove that lost candidate replies do not move the active data route.
# Usage: candidate-integration.sh ABSOLUTE_CRATE_DIR PRIVATE_KEY OUTPUT_DIR
set -eu
poc_crate=$1
poc_key=$2
poc_out=$3
mkdir -p "$poc_out"
chmod 700 "$poc_out"
export poc_crate poc_key poc_out
timeout 100 unshare -Urn sh -eu <<'NS'
ip link set lo up
poc_server=
poc_iperf=
cleanup() {
  rm -f "$poc_out/drop-probes"
  for poc_pid in "$poc_iperf" "$poc_server"; do
    if [ -n "$poc_pid" ]; then kill -TERM "$poc_pid" 2>/dev/null || true; wait "$poc_pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT INT TERM
export POC_TRACE_PATHS=1 POC_KEEPALIVE_SECS=5
export POC_TEST_DROP_PROBE_REPLIES_FILE="$poc_out/drop-probes"
"$poc_crate/target/release/rtp-l3-poc" server "$poc_key" rcand0 192.0.2.1/24 127.0.0.1:45983 45990-46000 30 2>"$poc_out/server.jsonl" &
poc_server=$!
poc_wait=0
until ip -o link show up dev rcand0 2>/dev/null | grep -q .; do
  kill -0 "$poc_server"
  poc_wait=$((poc_wait+1)); [ "$poc_wait" -lt 100 ]
  sleep .1
done
iperf3 -s -B 192.0.2.1 -p 45201 -J -i .5 >"$poc_out/iperf-server.json" 2>"$poc_out/iperf-server.stderr" &
poc_iperf=$!
cat >"$poc_out/app.sh" <<'APP'
set -eu
ping -n -c 3 -i .1 -W 2 192.0.2.1 >"$poc_out/startup.txt"
touch "$poc_out/drop-probes"
iperf3 -c 192.0.2.1 -p 45201 -u -b 5M -l 1000 -t 65 -i .5 --get-server-output -J >"$poc_out/udp.json" &
poc_transfer=$!
ping -n -i .1 -c 650 -W 1 192.0.2.1 >"$poc_out/loaded-ping.txt" &
poc_ping=$!
trap 'kill "$poc_transfer" "$poc_ping" 2>/dev/null || true; wait 2>/dev/null || true' EXIT INT TERM
python3 - <<'CONTROL'
import json, os, pathlib, time
out = pathlib.Path(os.environ['poc_out'])
deadline = time.monotonic()+56
while time.monotonic() < deadline:
    rows = [json.loads(v) for v in (out/'server.jsonl').read_text().splitlines() if v.startswith('{')]
    if sum(v['event'] == 'test_probe_reply_drop' for v in rows) >= 4:
        (out/'drop-probes').unlink()
        break
    time.sleep(.05)
else:
    raise RuntimeError('no exhausted candidate tuple observed')
CONTROL
wait "$poc_transfer"
wait "$poc_ping"
trap - EXIT INT TERM
APP
python3 "$poc_crate/scripts/client-netns.py" "$poc_crate/target/release/rtp-l3-poc" "$poc_key" 127.0.0.1:45983 rcand1 192.0.2.2/24 "$poc_out/app.sh" >"$poc_out/client.stdout" 2>"$poc_out/client.jsonl"
kill -TERM "$poc_server"
wait "$poc_server"
poc_server=
python3 - <<'CHECK'
import json, os, pathlib, re
out = pathlib.Path(os.environ['poc_out'])
def events(name):
    return [json.loads(v) for v in (out/name).read_text().splitlines() if v.startswith('{')]
client = events('client.jsonl')
server = events('server.jsonl')
final = client[-1]
assert final['event'] == 'final', final
assert final['fallbacks'] == 0, final
assert final['candidate_retries'] >= 1 and final['socket_renewals'] >= 3, final
assert final['switches'] >= 2, final
assert sum(v['event'] == 'test_probe_reply_drop' for v in server) >= 4
transfer = json.loads((out/'udp.json').read_text())
assert 'error' not in transfer, transfer.get('error')
intervals = transfer['server_output_json']['intervals']
assert len(intervals) >= 100 and all(v['sum']['bytes'] > 0 for v in intervals)
sent, received = map(int, re.search(r'(\d+) packets transmitted, (\d+) received', (out/'loaded-ping.txt').read_text()).groups())
assert received == sent == 650, (sent, received)
summary = dict(candidate_reply_drops=sum(v['event'] == 'test_probe_reply_drop' for v in server),
               candidate_retries=final['candidate_retries'], socket_renewals=final['socket_renewals'],
               fallbacks=final['fallbacks'], switches=final['switches'], ping_received=received,
               receiver_intervals=len(intervals), all_receiver_intervals_nonempty=True)
(out/'summary.json').write_text(json.dumps(summary, indent=2))
print(json.dumps(summary))
CHECK
NS
