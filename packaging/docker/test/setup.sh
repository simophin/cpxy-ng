#!/bin/sh
# Checks /etc/cpxy/setup on its own: what it writes for the image's defaults and for changed
# settings, and which settings it refuses. Needs only a POSIX shell.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
setup="$here/../rootfs/etc/cpxy/setup"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

# The image's defaults, as in Dockerfile.bundle
defaults() {
	env -i PATH="$PATH" CPXY_RUN="$work/run" \
		SERVER='https://:secret@example.com' \
		HTTP_PROXY_LISTEN='[::]:5000' SOCKS5_PROXY_LISTEN= "$@"
}

ok() {
	defaults "$@" sh "$setup" >"$work/out" 2>&1 || { cat "$work/out" >&2; fail "refused: $*"; }
}

refused() {
	local why="$1"
	shift
	if defaults "$@" sh "$setup" >"$work/out" 2>&1; then
		fail "accepted: $*"
	fi
	grep -q "$why" "$work/out" || { cat "$work/out" >&2; fail "refused for another reason: $*"; }
}

value() {
	[ "$(cat "$work/run/env/$1")" = "$2" ] || fail "$1 is '$(cat "$work/run/env/$1")', not '$2'"
}

ok
value client/CONFIG 'https://:secret@example.com'
[ ! -s "$work/run/env/client/SOCKS5_PROXY_LISTEN" ] || fail "SOCKS5_PROXY_LISTEN is not unset"
value tsgw/HTTPS_PROXY http://127.0.0.1:5000
value tsgw/HTTP_PROXY http://127.0.0.1:5000
[ ! -e "$work/run/env/tsgw/DNS_ALTERNATIVE" ] || fail "DNS_ALTERNATIVE is not left to the container's setting"
[ "$(stat -c %a "$work/run/env/client/CONFIG" 2>/dev/null || stat -f %Lp "$work/run/env/client/CONFIG")" = 600 ] ||
	fail "the server key is readable by others"
echo "defaults: ok"

# A second container on host network
ok HTTP_PROXY_LISTEN=0.0.0.0:5001 SOCKS5_PROXY_LISTEN='[::]:1081'
value tsgw/HTTPS_PROXY http://127.0.0.1:5001
value client/SOCKS5_PROXY_LISTEN '[::]:1081'
ok HTTP_PROXY_LISTEN=192.0.2.1:5000
value tsgw/HTTPS_PROXY http://192.0.2.1:5000
echo "changed settings: ok"

refused 'SERVER is required' SERVER=
refused 'HTTP_PROXY_LISTEN must be host:port' HTTP_PROXY_LISTEN=5000
refused 'HTTP_PROXY_LISTEN has no valid port' HTTP_PROXY_LISTEN='[::]:70000'
refused 'SOCKS5_PROXY_LISTEN must be host:port' SOCKS5_PROXY_LISTEN=::1:1080
echo "refused settings: ok"
