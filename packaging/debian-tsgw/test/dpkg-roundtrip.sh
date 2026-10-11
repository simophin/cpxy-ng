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
for f in /usr/bin/cpxy-tsgw /usr/lib/systemd/system/cpxy-tsgw.service /etc/cpxy/cpxy-tsgw.conf; do
	[ -e "$f" ] || fail "missing $f"
done
[ "$(stat -c %a /etc/cpxy/cpxy-tsgw.conf)" = 600 ] || fail "cpxy-tsgw.conf is readable by others"
# The binary runs here and refuses to start without SERVER
/usr/bin/cpxy-tsgw --config /etc/cpxy/cpxy-tsgw.conf 2>/tmp/err && fail "started without SERVER"
grep -q "SERVER is required" /tmp/err || fail "unexpected error: $(cat /tmp/err)"

# An edited config survives reinstalling (dpkg keeps a changed conffile)
sed -i "s|^SERVER=\$|SERVER=http://:k@example.com|" /etc/cpxy/cpxy-tsgw.conf
dpkg -i /pkg.deb >/dev/null 2>&1 || fail "reinstall"
grep -q "^SERVER=http://:k@example.com" /etc/cpxy/cpxy-tsgw.conf || fail "reinstall lost the config"

mkdir -p /var/lib/private/cpxy-tsgw && touch /var/lib/private/cpxy-tsgw/tailscaled.state
apt-get remove -y -qq cpxy-tsgw >/dev/null 2>&1 || fail "remove"
[ ! -e /usr/bin/cpxy-tsgw ] || fail "remove left the binary"
[ -e /etc/cpxy/cpxy-tsgw.conf ] || fail "remove deleted the config"
[ -e /var/lib/private/cpxy-tsgw/tailscaled.state ] || fail "remove deleted the node state"
apt-get purge -y -qq cpxy-tsgw >/dev/null 2>&1 || fail "purge"
[ ! -e /etc/cpxy/cpxy-tsgw.conf ] || fail "purge left the config"
[ ! -e /var/lib/private/cpxy-tsgw ] || fail "purge left the node state"
echo "ok"
'

for image in "$@"; do
	printf '%s: ' "$image"
	"$engine" run --rm -v "$deb:/pkg.deb:ro" "$image" sh -c "$script"
done
