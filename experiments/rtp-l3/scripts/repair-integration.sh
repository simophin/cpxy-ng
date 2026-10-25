#!/bin/sh
# Lose a previously working tuple and repair on the same fixed server port.
set -eu
poc_crate=$1 poc_key=$2 poc_out=$3
mkdir -p "$poc_out"
chmod 700 "$poc_out"
export poc_crate poc_key poc_out
timeout 50 unshare -Urn sh -eu <<'NS'
ip link set lo up
poc_server= poc_iperf=
cleanup() {
  rm -f "$poc_out/block-active"
  for poc_pid in "$poc_iperf" "$poc_server"; do
    if [ -n "$poc_pid" ]; then kill -TERM "$poc_pid" 2>/dev/null || true; wait "$poc_pid" 2>/dev/null || true; fi
  done
}
trap cleanup EXIT INT TERM
export POC_TRACE_PATHS=1 POC_TEST_DROP_ACTIVE_PATH_FILE="$poc_out/block-active"
"$poc_crate/target/release/rtp-l3-poc" server "$poc_key" rrepair0 192.0.2.1/24 127.0.0.1:45983 45990-46000 0 2>"$poc_out/server.jsonl" &
poc_server=$!
poc_wait=0
until ip -o link show up dev rrepair0 2>/dev/null | grep -q .; do
  kill -0 "$poc_server"
  poc_wait=$((poc_wait+1)); [ "$poc_wait" -lt 100 ]
  sleep .1
done
iperf3 -s -B 192.0.2.1 -p 45201 -J -i .5 >"$poc_out/iperf-server.json" 2>"$poc_out/iperf-server.stderr" &
poc_iperf=$!
cat >"$poc_out/app.sh" <<'APP'
set -eu
ping -n -c 3 -i .1 -W 2 192.0.2.1 >"$poc_out/startup.txt"
iperf3 -c 192.0.2.1 -p 45201 -u -b 5M -l 1000 -t 16 -i .5 --get-server-output -J >"$poc_out/udp.json" &
poc_transfer=$!
ping -n -i .1 -c 160 -W 1 192.0.2.1 >"$poc_out/loaded-ping.txt" &
poc_ping=$!
trap 'kill "$poc_transfer" "$poc_ping" 2>/dev/null || true; wait 2>/dev/null || true' EXIT INT TERM
sleep 3
touch "$poc_out/block-active"
wait "$poc_transfer"
wait "$poc_ping" || true
trap - EXIT INT TERM
APP
python3 "$poc_crate/scripts/client-netns.py" "$poc_crate/target/release/rtp-l3-poc" "$poc_key" 127.0.0.1:45983 rrepair1 192.0.2.2/24 "$poc_out/app.sh" >"$poc_out/client.stdout" 2>"$poc_out/client.jsonl"
kill -TERM "$poc_server"
wait "$poc_server"
poc_server=
python3 - <<'CHECK'
import json,os,pathlib
out=pathlib.Path(os.environ['poc_out'])
def events(name):return [json.loads(v) for v in (out/name).read_text().splitlines() if v.startswith('{')]
client,server=events('client.jsonl'),events('server.jsonl')
final=client[-1]
assert final['event']=='final' and final['fallbacks']==0,final
assert final['active_repairs']>=1 and final['switches']>=2,final
assert any(v['event']=='registration' and v['same_port'] and v['tuple_changed'] for v in server)
assert sum(v['event']=='test_active_path_blocked' for v in server)==1
transfer=json.loads((out/'udp.json').read_text())
assert 'error' not in transfer,transfer.get('error')
intervals=transfer['server_output_json']['intervals']
empty=maximum=0
for row in intervals:
    value=row['sum'];empty=empty+value['seconds'] if value['bytes']==0 else 0
    maximum=max(maximum,empty)
bound=maximum+2*max(v['sum']['seconds'] for v in intervals)
assert bound<5,bound
assert intervals[-1]['sum']['bytes']>0
summary=dict(active_repairs=final['active_repairs'],fallbacks=final['fallbacks'],
             socket_renewals=final['socket_renewals'],same_server_port_authenticated_rebind=True,
             receiver_stall_upper_bound_seconds=bound)
(out/'summary.json').write_text(json.dumps(summary,indent=2))
print(json.dumps(summary))
CHECK
NS
