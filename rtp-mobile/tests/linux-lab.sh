#!/usr/bin/env bash
# v2 intentionally defers mobile HTTP/protected TCP/UUID integration.
# Check the explicit failure boundary before profile, TUN, DNS or socket setup.
set -euo pipefail
mobile=${RTP_MOBILE_BIN:-target/debug/rtp-mobile-linux}
work=$(mktemp -d /tmp/rtp-mobile-unsupported-XXXX)
trap 'rm -rf "$work"' EXIT
if "$mobile" run "$work/nonexistent.json" rtpc lo "$work/control" >"$work/out" 2>"$work/err"; then
    echo 'mobile unexpectedly accepted the unsupported protocol' >&2
    exit 1
fi
grep -q 'HTTP-controlled RTP v2.*unsupported' "$work/err"
echo MOBILE_V2_UNSUPPORTED_OK
