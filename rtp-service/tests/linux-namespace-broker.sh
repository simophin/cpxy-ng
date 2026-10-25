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
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
"$BIN" endpoint --state-dir "$D/endpoint" --interface rtpe --listen 127.0.0.1:39000 \
    --rotation-ports 39001-39008 --rotation-seconds 2 --firewall none > "$D/endpoint.log" 2>&1 &
endpoint=$!
for _ in $(seq 1 100); do
    grep -Eq '"event":"ready"' "$D/endpoint.log" && break
    kill -0 "$endpoint"
    sleep .1
done
grep -Eq '"event":"ready"' "$D/endpoint.log"
# The application can bind no host loopback, and its only IPv4 route is the TUN.
"$BIN" client --state-dir "$D/client" --interface rtpc --outbound-interface lo \
    --entry 127.0.0.1:39000 --bind 127.0.0.1:0 namespace -- sh -c '
    test "$(ip -o -4 route show | wc -l)" -eq 1
    test -z "$(ip -o -4 addr show dev lo)"
    test -z "${CPXY_RTP_PASSWORD:-}"
    ping -n -i .2 -c 60 -W 2 10.88.0.1
' > "$D/client.log" 2>&1 &
client=$!
wait "$client" || { cat "$D/client.log"; exit 1; }
client=
grep -Eq '"event":"connected"' "$D/client.log"
grep -Eq '"event":"switched"|"switches":[1-9]' "$D/client.log"
grep -Eq '"path_candidates":[2-9]|"path_candidates":[1-9][0-9]' "$D/client.log"
grep -Eq '0% packet loss' "$D/client.log"
[[ ! -e /sys/class/net/rtpc ]]
kill -TERM "$endpoint"
wait "$endpoint"
endpoint=
[[ ! -e /sys/class/net/rtpe ]]
