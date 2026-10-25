#!/usr/bin/env bash
# Managed CLI, real TUN/UDP and candidate replacements. No host route mutations.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN="$ROOT/target/debug/cpxy-rtp"
if [[ ${1:-} != __inner ]]; then
    before=$(ip -4 -j route show table all; ip -4 -j rule show)
    unshare -Urnm bash "$0" __inner
    after=$(ip -4 -j route show table all; ip -4 -j rule show)
    [[ "$before" == "$after" ]]
    echo 'namespace broker: managed app and candidate replacement passed; host routes unchanged'
    exit
fi
D=$(mktemp -d)
endpoint=
client=
cleanup() {
    [[ -z "$client" ]] || kill -TERM "$client" 2>/dev/null || true
    [[ -z "$endpoint" ]] || kill -TERM "$endpoint" 2>/dev/null || true
    [[ -z "$client" ]] || wait "$client" 2>/dev/null || true
    [[ -z "$endpoint" ]] || wait "$endpoint" 2>/dev/null || true
    rm -rf "$D"
}
trap 'status=$?; if [[ $status -ne 0 ]]; then cat "$D"/*.log 2>/dev/null || true; fi; cleanup; exit "$status"' EXIT
ip link set lo up
umask 077
head -c 32 /dev/urandom > "$D/key"
cat > "$D/endpoint.json" <<JSON
{"profile":"broker-endpoint","key_file":"$D/key","journal":"$D/endpoint.journal","interface":"rtpe","address":"10.88.0.1/24","transport":{"role":"endpoint","listen":"127.0.0.1:39000","rotation_low":39001,"rotation_high":39008,"rotation_seconds":2},"admission":"administrator_managed"}
JSON
cat > "$D/client.json" <<JSON
{"profile":"broker-client","key_file":"$D/key","journal":"$D/client.journal","interface":"rtpc","address":"10.88.0.2/24","outbound_interface":"lo","transport":{"role":"client","entry":"127.0.0.1:39000","bind":"127.0.0.1:0"},"admission":"administrator_managed"}
JSON
"$BIN" run "$D/endpoint.json" > "$D/endpoint.log" 2>&1 &
endpoint=$!
for _ in $(seq 1 100); do
    rg -q '"event":"ready"' "$D/endpoint.log" && break
    kill -0 "$endpoint"
    sleep .1
done
rg -q '"event":"ready"' "$D/endpoint.log"
# The application can bind no host loopback, and its only IPv4 route is the TUN.
"$BIN" namespace-run "$D/client.json" -- sh -c '
    test "$(ip -o -4 route show | wc -l)" -eq 1
    test -z "$(ip -o -4 addr show dev lo)"
    ping -n -i .2 -c 60 -W 2 10.88.0.1
' > "$D/client.log" 2>&1 &
client=$!
wait "$client" || { cat "$D/client.log"; exit 1; }
client=
rg -q '"event":"connected"' "$D/client.log"
rg -q '"event":"switched"|"switches":[1-9]' "$D/client.log"
rg -q '"path_candidates":[2-9]|"path_candidates":[1-9][0-9]' "$D/client.log"
rg -q '0% packet loss' "$D/client.log"
[[ ! -e /sys/class/net/rtpc ]]
kill -TERM "$endpoint"
wait "$endpoint"
endpoint=
[[ ! -e /sys/class/net/rtpe ]]
