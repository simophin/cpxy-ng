#!/usr/bin/env bash
# Interleaved ref/A/B router windows: idle, fixed-rate and saturating downloads through rtpr0.
set -euo pipefail
# Usage: BINS=DIR router-windows.sh RESULTS_DIR. The live router binary is the reference.
# Requires an operator-provisioned origin at $URL bound to the endpoint's tunnel address,
# serving an 80,000,000-byte f80. Restarts the router service once per window and leaves
# the reference binary installed; a rollback copy is kept in /root.
: "${BINS:?set BINS to a directory holding router-A-counters and router-B-poll}"
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
OUT=${1:?results directory}
R=root@tr3600
URL=http://10.88.0.1:18080/f80
mkdir -p "$OUT"
ssh_r() { ssh -o BatchMode=yes -o ConnectTimeout=10 "$R" "$@"; }

ssh_r 'mkdir -p /tmp/rtpbench && cp /usr/bin/cpxy-rtp-router /tmp/rtpbench/ref &&
  [ -e /root/cpxy-rtp-router-before-counters-20261028 ] || cp /usr/bin/cpxy-rtp-router /root/cpxy-rtp-router-before-counters-20261028'
ssh_r "cat > /tmp/rtpbench/A && chmod 755 /tmp/rtpbench/A" < "$BINS/router-A-counters"
ssh_r "cat > /tmp/rtpbench/B && chmod 755 /tmp/rtpbench/B" < "$BINS/router-B-poll"
ssh_r 'sha256sum /tmp/rtpbench/* /usr/bin/cpxy-rtp-router' | tee "$OUT/hashes.txt"

swap() {
    ssh_r "/etc/init.d/cpxy-rtp-router stop; cp /tmp/rtpbench/$1 /usr/bin/cpxy-rtp-router; /etc/init.d/cpxy-rtp-router start"
    for _ in $(seq 1 60); do
        if ssh_r 'ip route replace 10.88.0.1/32 dev rtpr0 2>/dev/null && ping -c 1 -W 2 -I rtpr0 10.88.0.1 >/dev/null 2>&1'; then
            sleep 15
            ssh_r 'ip route replace 10.88.0.1/32 dev rtpr0'
            return 0
        fi
        sleep 1
    done
    echo "tunnel did not come up for $1" >&2
    return 1
}

# phase NAME SAMPLES PINGS LOAD_COMMAND
phase() {
    local tag=$1 samples=$2 pings=$3 load=$4 pid
    pid=$(ssh_r 'pidof cpxy-rtp-router')
    python3 -I "$REPO/experiments/rtp-l3/scripts/profile-packet-path.py" "$pid" --ssh "$R" \
        --samples "$samples" > "$OUT/$tag.profile.jsonl" &
    local profiler=$!
    sleep 2
    ssh_r "logread | grep '\"state\"' | tail -n 1 | sed 's/^/STATUS_BEFORE /'
        echo LOAD_START \$(cut -d' ' -f1 /proc/uptime)
        ping -c $pings -I rtpr0 10.88.0.1 > /tmp/rtpbench/ping.txt 2>&1 &
        $load
        echo LOAD_END \$(cut -d' ' -f1 /proc/uptime)
        wait
        sed 's/^/PING /' /tmp/rtpbench/ping.txt | tail -n 3
        sleep 11
        logread | grep '\"state\"' | tail -n 1 | sed 's/^/STATUS_AFTER /'" > "$OUT/$tag.run.txt"
    wait "$profiler"
}

curls() {
    # $1: curl rate option, possibly empty.
    echo "for n in 1 2 3 4; do curl -4 --fail -sS --interface rtpr0 $1 --max-time 90 -o /dev/null \
      -w \"RESULT stream=\$n code=%{http_code} bytes=%{size_download} seconds=%{time_total}\n\" $URL & done; wait"
}

round=0
for bin in ref A B ref A B ref A B ref; do
    round=$((round + 1))
    echo "== window $round: $bin $(date +%T)"
    swap "$bin"
    phase "w$round-$bin-idle" 32 28 "sleep 30"
    phase "w$round-$bin-fixed" 38 30 "$(curls '--limit-rate 2500k')"
    phase "w$round-$bin-sat" 34 20 "$(curls '')"
done
ssh_r 'sha256sum /usr/bin/cpxy-rtp-router; ip route del 10.88.0.1/32 dev rtpr0 || true' | tee -a "$OUT/hashes.txt"
ssh_r 'rm -rf /tmp/rtpbench'
echo DONE
