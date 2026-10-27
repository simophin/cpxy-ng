#!/bin/sh
# Run on the router after provisioning a private HTTP origin and loopback CPXY client.
# Does not change services, routes, firewall rules or credentials.
set -eu
: "${BENCH_URL:?set BENCH_URL to a controlled download URL}"
BENCH_PROXY=${BENCH_PROXY:-http://127.0.0.1:15000}
BENCH_INTERFACE=${BENCH_INTERFACE:-rtpr0}
BENCH_ROUNDS=${BENCH_ROUNDS:-3}
case "$BENCH_ROUNDS" in ''|*[!0-9]*) exit 2;; esac
[ "$BENCH_ROUNDS" -ge 1 ] && [ "$BENCH_ROUNDS" -le 10 ] || exit 2
snapshot() {
    echo "uptime=$(cut -d ' ' -f1 /proc/uptime)"
    grep -E '^Udp6RcvbufErrors|^Ip6FragFails' /proc/net/snmp6 || true
    grep -A1 '^Udp:' /proc/net/snmp || true
}
for streams in 1 4; do
    for round in $(seq 1 "$BENCH_ROUNDS"); do
        for mode in proxy rtp; do
            case "$mode" in
                proxy) set -- --proxy "$BENCH_PROXY";;
                rtp) set -- --interface "$BENCH_INTERFACE";;
            esac
            echo "BEGIN mode=$mode streams=$streams round=$round"
            snapshot
            pids=''
            for n in $(seq 1 "$streams"); do
                curl -4 --fail -sS "$@" --connect-timeout 8 --max-time 60 -o /dev/null \
                    -w "RESULT mode=$mode streams=$streams round=$round stream=$n code=%{http_code} bytes=%{size_download} speed_Bps=%{speed_download} seconds=%{time_total}\n" \
                    "$BENCH_URL" &
                pids="$pids $!"
            done
            failed=0
            for pid in $pids; do wait "$pid" || failed=1; done
            echo "END mode=$mode streams=$streams round=$round failed=$failed"
            snapshot
            [ "$failed" = 0 ] || exit 1
        done
    done
done
