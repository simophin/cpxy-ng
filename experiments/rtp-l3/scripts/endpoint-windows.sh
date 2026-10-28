#!/usr/bin/env bash
# Interleaved endpoint windows (ref/new/...): idle, fixed-rate and saturating downloads from
# the router through rtpr0, profiling the router client and the endpoint together.
# Usage: [ENDPOINT_BIN=PATH] [SEQUENCE="ref new ..."] endpoint-windows.sh RESULTS_DIR
# The live endpoint binary is the reference, reinstalled by each `ref` window; `live` keeps
# whatever is installed. A rollback copy is kept in /root. Restarts the endpoint, then the router, once per window. Serves a
# temporary origin bound only to the endpoint's tunnel address, removed afterwards.
set -euo pipefail
SEQUENCE=${SEQUENCE:-ref new ref new ref new ref}
case " $SEQUENCE " in *" new "*) : "${ENDPOINT_BIN:?set ENDPOINT_BIN to the candidate endpoint binary}";; esac
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
OUT=${1:?results directory}
R=root@tr3600
E=root@hh-hk
URL=http://10.88.0.1:18080/f80
mkdir -p "$OUT"
on() { local host=$1; shift; ssh -o BatchMode=yes -o ConnectTimeout=10 "$host" "$@"; }

on "$E" 'mkdir -p /tmp/rtpbench && cp /usr/local/bin/cpxy-rtp /tmp/rtpbench/ref &&
  { [ -e /root/cpxy-rtp-before-async-20261028 ] || cp /usr/local/bin/cpxy-rtp /root/cpxy-rtp-before-async-20261028; } &&
  truncate -s 80000000 /tmp/rtpbench/f80'
if [ -n "${ENDPOINT_BIN:-}" ]; then
    on "$E" "cat > /tmp/rtpbench/new && chmod 755 /tmp/rtpbench/new" < "$ENDPOINT_BIN"
fi
on "$E" 'sha256sum /tmp/rtpbench/ref; [ ! -e /tmp/rtpbench/new ] || sha256sum /tmp/rtpbench/new' | tee "$OUT/hashes.txt"
on "$R" 'sha256sum /usr/bin/cpxy-rtp-router' | tee -a "$OUT/hashes.txt"

swap() {
    local install=''
    [ "$1" = live ] || install="cp /tmp/rtpbench/$1 /usr/local/bin/cpxy-rtp;"
    on "$E" "systemctl stop cpxy-rtp; $install systemctl start cpxy-rtp
        for i in \$(seq 1 30); do ip -br addr show rtp0 2>/dev/null | grep -q 10.88.0.1 && break; sleep 1; done
        [ ! -e /tmp/rtpbench/origin.pid ] || kill \$(cat /tmp/rtpbench/origin.pid) 2>/dev/null || true
        cd /tmp/rtpbench && { nohup timeout 1h python3 -m http.server --bind 10.88.0.1 18080 >/dev/null 2>&1 & echo \$! > origin.pid; }"
    on "$R" '/etc/init.d/cpxy-rtp-router restart'
    for _ in $(seq 1 60); do
        if on "$R" 'ip route replace 10.88.0.1/32 dev rtpr0 2>/dev/null && ping -c 1 -W 2 -I rtpr0 10.88.0.1 >/dev/null 2>&1'; then
            sleep 15
            on "$R" 'ip route replace 10.88.0.1/32 dev rtpr0'
            return 0
        fi
        sleep 1
    done
    echo "tunnel did not come up for $1" >&2
    return 1
}

# phase NAME SAMPLES PINGS LOAD_COMMAND
phase() {
    local tag=$1 samples=$2 pings=$3 load=$4 router_pid endpoint_pid
    router_pid=$(on "$R" 'pidof cpxy-rtp-router')
    endpoint_pid=$(on "$E" 'systemctl show cpxy-rtp -p MainPID --value')
    python3 -I "$REPO/experiments/rtp-l3/scripts/profile-packet-path.py" "$router_pid" --ssh "$R" \
        --samples "$samples" > "$OUT/$tag.profile.jsonl" &
    local router_profiler=$!
    python3 -I "$REPO/experiments/rtp-l3/scripts/profile-packet-path.py" "$endpoint_pid" --ssh "$E" \
        --samples "$samples" > "$OUT/$tag.endpoint.jsonl" &
    local endpoint_profiler=$!
    sleep 2
    on "$R" "logread | grep '\"state\"' | tail -n 1 | sed 's/^/STATUS_BEFORE /'
        echo LOAD_START \$(cut -d' ' -f1 /proc/uptime)
        ping -c $pings -I rtpr0 10.88.0.1 > /tmp/ping.txt 2>&1 &
        $load
        echo LOAD_END \$(cut -d' ' -f1 /proc/uptime)
        wait
        sed 's/^/PING /' /tmp/ping.txt | tail -n 3
        sleep 11
        logread | grep '\"state\"' | tail -n 1 | sed 's/^/STATUS_AFTER /'" > "$OUT/$tag.run.txt"
    wait "$router_profiler" "$endpoint_profiler"
}

curls() {
    echo "for n in 1 2 3 4; do curl -4 --fail -sS --interface rtpr0 $1 --max-time 90 -o /dev/null \
      -w \"RESULT stream=\$n code=%{http_code} bytes=%{size_download} seconds=%{time_total}\n\" $URL & done; wait"
}

round=0
for bin in $SEQUENCE; do
    round=$((round + 1))
    echo "== window $round: $bin $(date +%T)"
    swap "$bin"
    phase "w$round-$bin-idle" 32 28 "sleep 30"
    phase "w$round-$bin-fixed" 38 30 "$(curls '--limit-rate 2500k')"
    phase "w$round-$bin-sat" 34 20 "$(curls '')"
done
on "$E" 'kill $(cat /tmp/rtpbench/origin.pid) 2>/dev/null || true; sha256sum /usr/local/bin/cpxy-rtp; rm -rf /tmp/rtpbench' | tee -a "$OUT/hashes.txt"
on "$R" 'ip route del 10.88.0.1/32 dev rtpr0 || true; rm -f /tmp/ping.txt'
echo DONE
