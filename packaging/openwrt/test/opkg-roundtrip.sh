#!/bin/sh
# Install the ipk into an OpenWrt 24.10 rootfs container, check what it changed, remove it, and
# check the configuration is back to how it started.
#
# usage: opkg-roundtrip.sh <ipk> [image]    (needs podman or docker)
# Does not start the service: the container has no procd, netifd or fw4. See lab.sh for the
# routing behaviour.
set -eu

ipk="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
image="${2:-docker.io/openwrt/rootfs:x86-64-openwrt-24.10}"
engine="$(command -v podman || command -v docker)"

script='
set -eu
# Compare settings, not file text: `uci commit` rewrites a file with normalised quoting and no comments
snapshot() { for f in $(ls /etc/config); do echo "== $f"; uci export "$f"; done; ls /etc/rc.d /etc/init.d; }
mkdir -p /var/lock /tmp
# Offline: this tests what the package changes, not dependency resolution, so mark the
# dependencies (ip-full, kmod-tun) as already installed
for dep in ip-full kmod-tun; do
	printf "Package: %s\nVersion: 1\nStatus: install ok installed\nArchitecture: x86_64\n\n" "$dep" >>/usr/lib/opkg/status
	: >"/usr/lib/opkg/info/$dep.list"
done
snapshot >/before.txt

opkg install /pkg.ipk

# What the install added
for f in /usr/bin/cpxy-client /usr/bin/cpxy-dns-split /usr/bin/cpxy-tun2proxy \
	/etc/init.d/cpxy /etc/config/cpxy /usr/libexec/cpxy/net.sh /usr/libexec/cpxy/dns-split.sh \
	/usr/libexec/cpxy/fw3-include.sh \
	/usr/share/nftables.d/chain-pre/forward/10-cpxy.nft; do
	[ -e "$f" ] || { echo "FAIL: missing $f"; exit 1; }
done
[ "$(uci get firewall.cpxy_zone.name)" = cpxy ] || { echo "FAIL: firewall zone not added"; exit 1; }
[ "$(uci get firewall.cpxy_fwd.dest)" = cpxy ] || { echo "FAIL: forwarding not added"; exit 1; }
# fw4 reads the nftables snippet; the iptables include is for fw3 only
uci -q get firewall.cpxy_udp && { echo "FAIL: fw3 include added on fw4"; exit 1; }
[ -e /etc/uci-defaults/90-cpxy ] && { echo "FAIL: uci-defaults script was not consumed"; exit 1; }
ls /etc/rc.d | grep -q cpxy || { echo "FAIL: service not enabled"; exit 1; }
echo "install ok"

opkg remove cpxy-router
for f in /usr/bin/cpxy-client /usr/bin/cpxy-dns-split /usr/bin/cpxy-tun2proxy /etc/init.d/cpxy \
	/usr/libexec/cpxy /usr/share/nftables.d/chain-pre/forward/10-cpxy.nft /etc/uci-defaults/90-cpxy; do
	[ ! -e "$f" ] || { echo "FAIL: left behind $f"; exit 1; }
done
uci -q get firewall.cpxy_zone && { echo "FAIL: firewall zone left behind"; exit 1; }
uci -q get firewall.cpxy_fwd && { echo "FAIL: forwarding left behind"; exit 1; }
rm -f /etc/config/cpxy   # a modified conffile is kept by opkg on purpose; the default one is removed
snapshot >/after.txt
if [ "$(cat /before.txt)" = "$(cat /after.txt)" ]; then
	echo "remove ok: configuration restored"
else
	echo "FAIL: configuration differs after removal"; echo "--- before"; cat /before.txt; echo "--- after"; cat /after.txt
	exit 1
fi
'

"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
