#!/bin/sh
# Serve two builds of the ipk as signed opkg feeds, the way the download server does (each version
# its own feed, with `latest` moving between them), and check an OpenWrt 24.10 rootfs installs
# the older one, upgrades to the newer one through the feed the package lists, and rejects a
# tampered index.
#
# usage: feed.sh <older ipk> <newer ipk> [image]    (needs podman or docker, and node)
# Build the ipks with CPXY_FEED_URL=file:/// so the feed they list is file:///latest/openwrt/x86_64.
set -eu

[ $# -ge 2 ] || { sed -n '2,8p' "$0" >&2; exit 2; }
here="$(cd "$(dirname "$0")/.." && pwd)"
image="${3:-docker.io/openwrt/rootfs:x86-64-openwrt-24.10}"
engine="$(command -v podman || command -v docker)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A throwaway key; the package's own key is checked to be installed, not used
node "$here/usign-keygen.mjs" "$work/test.key" "$work/test.pub" >/dev/null
mkdir -p "$work/feeds/old" "$work/feeds/new"
cp "$1" "$work/feeds/old/cpxy.ipk"
cp "$2" "$work/feeds/new/cpxy.ipk"
for v in old new; do "$here/make-feed.sh" "$work/feeds/$v" "$work/test.key" >/dev/null; done
version() { tar -xzOf "$1" ./control.tar.gz | tar -xzO ./control | sed -n 's/^Version: //p'; }
old="$(version "$1")" new="$(version "$2")"
package_key="$(tail -1 "$here/feed.pub" | base64 -d | od -An -tx1 -j2 -N8 | tr -d ' \n')"

script='
set -eu
grep -q "^option check_signature" /etc/opkg.conf || { echo "FAIL: opkg does not check signatures"; exit 1; }
mkdir -p /var/lock /tmp
for dep in ip-full kmod-tun; do
	printf "Package: %s\nVersion: 1\nStatus: install ok installed\nArchitecture: x86_64\n\n" "$dep" >>/usr/lib/opkg/status
	: >"/usr/lib/opkg/info/$dep.list"
done
# Offline: only the test feed, added by hand for the first install as the docs say
: >/etc/opkg/distfeeds.conf
echo "src/gz cpxy file:///latest/openwrt/x86_64" >/etc/opkg/customfeeds.conf
cp /test.pub "/etc/opkg/keys/$(usign -F -p /test.pub)"

mkdir -p /latest/openwrt
ln -s /feeds/old /latest/openwrt/x86_64
opkg update
opkg install cpxy
opkg list-installed cpxy | grep -qx "cpxy - $OLD" || { echo "FAIL: expected $OLD"; opkg list-installed; exit 1; }
[ -f "/etc/opkg/keys/$PACKAGE_KEY" ] || { echo "FAIL: the package did not install its feed key"; exit 1; }
echo "install from feed ok: $OLD"

# From now on only the feed the package lists
grep -qx "src/gz cpxy file:///latest/openwrt/x86_64" /etc/opkg/cpxy.conf || { echo "FAIL: the package did not list its feed"; exit 1; }
: >/etc/opkg/customfeeds.conf
rm /latest/openwrt/x86_64 && ln -s /feeds/new /latest/openwrt/x86_64
opkg update
opkg upgrade cpxy
opkg list-installed cpxy | grep -qx "cpxy - $NEW" || { echo "FAIL: expected $NEW"; opkg list-installed; exit 1; }
echo "upgrade from feed ok: $NEW"

cp -R /feeds/new /tampered
sed -i "s/^Size: .*/Size: 1/" /tampered/Packages
usign -V -q -m /tampered/Packages -P /etc/opkg/keys && { echo "FAIL: tampered index verified"; exit 1; }
echo "tampered index rejected"
'

"$engine" run --rm -v "$work/feeds:/feeds:ro" -v "$work/test.pub:/test.pub:ro" \
	-e OLD="$old" -e NEW="$new" -e PACKAGE_KEY="$package_key" "$image" sh -c "$script"
