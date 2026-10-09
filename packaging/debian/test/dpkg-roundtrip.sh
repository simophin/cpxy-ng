#!/bin/sh
# Installs, upgrades, removes and purges the .deb in Debian and Ubuntu containers (no systemd
# there, so the maintainer scripts skip systemctl), checking files, permissions and the config.
#
# usage: dpkg-roundtrip.sh <deb> [image...]    (needs podman or docker)
set -eu

deb="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
shift
[ $# -gt 0 ] || set -- docker.io/library/debian:bookworm docker.io/library/debian:trixie docker.io/library/ubuntu:24.04
engine="$(command -v podman || command -v docker)"

script='
set -eu
fail() { echo "FAIL: $*"; exit 1; }
apt-get update -qq >/dev/null
apt-get install -y -qq /pkg.deb >/dev/null 2>&1 || fail "install"
for f in /usr/bin/cpxy-router /usr/bin/cpxy-dns-split /usr/libexec/cpxy/gateway.sh /usr/libexec/cpxy/dns.sh \
	/usr/libexec/cpxy/config.sh /usr/libexec/cpxy/net.sh /usr/lib/systemd/system/cpxy-router.service \
	/usr/lib/systemd/system/cpxy-dns-split.service /usr/lib/sysctl.d/60-cpxy.conf /etc/cpxy/cpxy.conf; do
	[ -e "$f" ] || fail "missing $f"
done
[ "$(stat -c %a /etc/cpxy/cpxy.conf)" = 600 ] || fail "cpxy.conf is readable by others"
# The config parses, and an unset SERVER is refused
/usr/libexec/cpxy/gateway.sh up 2>/tmp/err && fail "started without SERVER"
grep -q "SERVER is not set" /tmp/err || fail "unexpected error: $(cat /tmp/err)"

# An edited config survives reinstalling (dpkg keeps a changed conffile)
sed -i "s|^SERVER=\$|SERVER=http://:k@example.com|" /etc/cpxy/cpxy.conf
dpkg -i /pkg.deb >/dev/null 2>&1 || fail "reinstall"
grep -q "^SERVER=http://:k@example.com" /etc/cpxy/cpxy.conf || fail "reinstall lost the config"

apt-get remove -y -qq cpxy-gateway >/dev/null 2>&1 || fail "remove"
[ ! -e /usr/bin/cpxy-router ] || fail "remove left the binary"
[ -e /etc/cpxy/cpxy.conf ] || fail "remove deleted the config"
apt-get purge -y -qq cpxy-gateway >/dev/null 2>&1 || fail "purge"
[ ! -e /etc/cpxy/cpxy.conf ] || fail "purge left the config"
echo "ok"
'

for image in "$@"; do
	printf '%s: ' "$image"
	"$engine" run --rm -v "$deb:/pkg.deb:ro" "$image" sh -c "$script"
done
