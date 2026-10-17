#!/bin/sh
# Install the ipk into an OpenWrt 24.10 rootfs container, check what it changed, remove it, and
# check the configuration is back to how it started.
#
# usage: opkg-roundtrip.sh <ipk> [image]    (needs podman or docker)
# Does not run the services: the container has no procd, netifd or fw4. See init-dryrun.sh for
# the init scripts and lab.sh for the routing behaviour.
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
for f in /usr/bin/cpxy-router /usr/bin/cpxy-client /usr/bin/cpxy-tsgw \
	/usr/bin/cpxy-show /etc/config/cpxy /lib/upgrade/keep.d/cpxy \
	/usr/libexec/cpxy/net.sh /usr/libexec/cpxy/firewall.sh /usr/libexec/cpxy/instance.sh; do
	[ -e "$f" ] || { echo "FAIL: missing $f"; exit 1; }
done
for service in cpxy-client cpxy-router cpxy-tsgw; do
	[ -x /etc/init.d/$service ] || { echo "FAIL: missing /etc/init.d/$service"; exit 1; }
	ls /etc/rc.d | grep -q "^S..$service\$" || { echo "FAIL: $service not enabled"; exit 1; }
done
# Without a server nothing is configured, so the network is untouched
uci show firewall | grep -q cpxy_inst_ && { echo "FAIL: firewall changed before configuration"; exit 1; }
cpxy-show 2>/dev/null && { echo "FAIL: the default configuration must ask for a server"; exit 1; }
cpxy-show 2>&1 | grep -q "server" || { echo "FAIL: missing server not reported"; exit 1; }
uci set cpxy.defaults.server=https://:key@proxy.example:443
cpxy-show main | grep -q "HTTP \[::\]:5000" || { echo "FAIL: cpxy-show"; cpxy-show main; exit 1; }
uci revert cpxy
echo "install ok"

opkg remove cpxy
for f in /usr/bin/cpxy-router /usr/bin/cpxy-client /usr/bin/cpxy-tsgw \
	/usr/bin/cpxy-show /etc/init.d/cpxy-client /etc/init.d/cpxy-router /etc/init.d/cpxy-tsgw \
	/usr/libexec/cpxy; do
	[ ! -e "$f" ] || { echo "FAIL: left behind $f"; exit 1; }
done
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
