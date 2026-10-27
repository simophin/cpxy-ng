#!/usr/bin/env bash
# One dual-stack rotating UDP endpoint, two namespace clients, IPv4 inner packets.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${RTP_SERVICE_BIN:-$ROOT/target/debug/cpxy-rtp}
snapshot() {
    for family in -4 -6; do
        ip "$family" -j route show table all | python3 -c 'import json,sys; rows=json.load(sys.stdin); [r.pop("expires",None) for r in rows]; print(json.dumps(rows,sort_keys=True))'
        ip "$family" -j rule show
    done
}
if [[ ${1:-} != __inner ]]; then
    before=$(snapshot)
    unshare -Urnm bash "$0" __inner
    after=$(snapshot)
    [[ "$before" == "$after" ]] || { echo "Host route comparison failed" >&2; exit 1; }
    echo 'DUAL_STACK_OK: IPv4 payload, both outer families, HTTP control and protected broker sockets; host routes unchanged'
    exit
fi
D=$(mktemp -d)
endpoint=
standby=
clients=()
cleanup() {
    for pid in "${clients[@]}" "$endpoint" "$standby"; do
        [[ -z "$pid" ]] || kill -TERM "$pid" 2>/dev/null || true
    done
    for pid in "${clients[@]}" "$endpoint" "$standby"; do
        [[ -z "$pid" ]] || wait "$pid" 2>/dev/null || true
    done
    rm -rf "$D"
}
trap 'status=$?; if [[ $status -ne 0 ]]; then cat "$D"/*.log 2>/dev/null || true; fi; cleanup; exit "$status"' EXIT
ip link set lo up
umask 077
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
endpoint_args=(endpoint --state-dir "$D/endpoint" --interface rtpe --listen '[::]:39000'
    --udp-ports 39001-39008 --firewall none
    --internet-exit --route-table 18800 --rule-priority 1000)
"$BIN" "${endpoint_args[@]}" >"$D/endpoint.log" 2>&1 &
endpoint=$!
for _ in $(seq 1 100); do
    grep -q '"event":"ready"' "$D/endpoint.log" && break
    kill -0 "$endpoint"
    sleep .1
done
grep -q '"event":"ready"' "$D/endpoint.log"
# Start another endpoint with an earlier, disjoint client-return selector, then
# restart the first while that selector remains. Both restart orders must work.
"$BIN" endpoint --state-dir "$D/standby" --interface rtps --listen '[::]:39100' \
    --udp-ports 39101-39108 --firewall none --internet-exit \
    --address 10.89.0.1/24 --route-table 18830 \
    --rule-priority 900 >"$D/standby.log" 2>&1 &
standby=$!
for _ in $(seq 1 100); do
    grep -q '"event":"ready"' "$D/standby.log" && break
    kill -0 "$standby"
    sleep .1
done
grep -q '"event":"ready"' "$D/standby.log"
kill -TERM "$endpoint"
wait "$endpoint"
endpoint=
"$BIN" "${endpoint_args[@]}" >"$D/endpoint.log" 2>&1 &
endpoint=$!
for _ in $(seq 1 100); do
    grep -q '"event":"ready"' "$D/endpoint.log" && break
    kill -0 "$endpoint"
    sleep .1
done
grep -q '"event":"ready"' "$D/endpoint.log"
ip link set rtpe up
for family in 4 6; do
    if [[ "$family" == 4 ]]; then entry=127.0.0.1:39000; bind=127.0.0.1:0; address=10.88.0.2/24
    else entry='[::1]:39000'; bind='[::1]:0'; address=10.88.0.3/24; fi
    "$BIN" client --state-dir "$D/client$family" --interface rtpc --outbound-interface lo \
        --entry "$entry" --bind "$bind" --address "$address" namespace -- sh -c '
        test "$(ip -o -4 route show | wc -l)" -eq 1
        test -z "$(ip -o -6 addr show dev rtpc)"
        test -z "${CPXY_RTP_PASSWORD:-}"
        ping -4 -n -i .2 -c 40 -W 2 10.88.0.1
    ' >"$D/client$family.log" 2>&1 &
    clients+=("$!")
done
for pid in "${clients[@]}"; do wait "$pid"; done
clients=()
for family in 4 6; do
    grep -q '"event":"connected"' "$D/client$family.log"
    grep -q ', 0% packet loss' "$D/client$family.log"
done
kill -TERM "$endpoint"
wait "$endpoint"
endpoint=
[[ ! -e /sys/class/net/rtpe ]]
echo INNER_DUAL_STACK_OK
