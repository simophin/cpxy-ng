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
		HTTP_PROXY_LISTEN='[::]:5000' SOCKS5_PROXY_LISTEN= SMARTDNS_LISTEN=127.0.0.1:5336 \
		SMARTDNS_SERVERS=tcp://1.1.1.1,tcp://8.8.8.8 SMARTDNS_CACHE_SIZE=4096 \
		SMARTDNS_PREFETCH=1 SMARTDNS_SERVE_EXPIRED=1 "$@"
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

has() {
	grep -qxF "$2" "$work/run/$1" || { cat "$work/run/$1" >&2; fail "$1 lacks '$2'"; }
}

value() {
	[ "$(cat "$work/run/env/$1")" = "$2" ] || fail "$1 is '$(cat "$work/run/env/$1")', not '$2'"
}

ok
has smartdns.conf 'bind 127.0.0.1:5336'
has smartdns.conf 'bind-tcp 127.0.0.1:5336'
has smartdns.conf 'proxy-server http://127.0.0.1:5000 -name cpxy'
has smartdns.conf 'server tcp://1.1.1.1 -proxy cpxy'
has smartdns.conf 'server tcp://8.8.8.8 -proxy cpxy'
has smartdns.conf 'prefetch-domain yes'
has smartdns.conf 'speed-check-mode none'
value client/CONFIG 'https://:secret@example.com'
[ ! -s "$work/run/env/client/SOCKS5_PROXY_LISTEN" ] || fail "SOCKS5_PROXY_LISTEN is not unset"
value tsgw/HTTPS_PROXY http://127.0.0.1:5000
value tsgw/HTTP_PROXY http://127.0.0.1:5000
value tsgw/DNS_ALTERNATIVE 127.0.0.1:5336
[ "$(stat -c %a "$work/run/env/client/CONFIG" 2>/dev/null || stat -f %Lp "$work/run/env/client/CONFIG")" = 600 ] ||
	fail "the server key is readable by others"
echo "defaults: ok"

# A second container on host network
ok HTTP_PROXY_LISTEN=0.0.0.0:5001 SOCKS5_PROXY_LISTEN='[::]:1081' SMARTDNS_LISTEN='[::1]:5346' \
	SMARTDNS_SERVERS='tls://1.1.1.1 https://[2606:4700:4700::1111]/dns-query' SMARTDNS_PREFETCH=0
has smartdns.conf 'bind [::1]:5346'
has smartdns.conf 'proxy-server http://127.0.0.1:5001 -name cpxy'
has smartdns.conf 'server tls://1.1.1.1 -proxy cpxy'
has smartdns.conf 'server https://[2606:4700:4700::1111]/dns-query -proxy cpxy'
has smartdns.conf 'prefetch-domain no'
value client/SOCKS5_PROXY_LISTEN '[::]:1081'
value tsgw/DNS_ALTERNATIVE '[::1]:5346'
ok HTTP_PROXY_LISTEN=192.0.2.1:5000
value tsgw/HTTPS_PROXY http://192.0.2.1:5000
echo "changed settings: ok"

refused 'SERVER is required' SERVER=
refused 'HTTP_PROXY_LISTEN must be host:port' HTTP_PROXY_LISTEN=5000
refused 'HTTP_PROXY_LISTEN has no valid port' HTTP_PROXY_LISTEN='[::]:70000'
refused 'SOCKS5_PROXY_LISTEN must be host:port' SOCKS5_PROXY_LISTEN=::1:1080
refused 'SMARTDNS_LISTEN must be on loopback' SMARTDNS_LISTEN=0.0.0.0:5336
refused 'SMARTDNS_LISTEN must not use port 53' SMARTDNS_LISTEN=127.0.0.53:53
refused 'must use different ports' SMARTDNS_LISTEN=127.0.0.1:5000
refused 'must use different ports' SOCKS5_PROXY_LISTEN='[::]:5336'
refused 'SMARTDNS_SERVERS is empty' SMARTDNS_SERVERS=
refused "'udp://1.1.1.1' must be" SMARTDNS_SERVERS=udp://1.1.1.1
refused "'1.1.1.1' must be" SMARTDNS_SERVERS=1.1.1.1
refused "'tls://dns.google' must be" SMARTDNS_SERVERS=tls://dns.google
refused 'SMARTDNS_CACHE_SIZE must be a number' SMARTDNS_CACHE_SIZE=lots
refused 'SMARTDNS_SERVE_EXPIRED must be 0 or 1' SMARTDNS_SERVE_EXPIRED=yes
echo "refused settings: ok"
