#!/bin/sh
# Real OpenWrt apk hooks, using stand-in binaries; no router is contacted.
# Requires docker/podman. Optional first argument selects the OpenWrt rootfs.
set -eu
here="$(cd "$(dirname "$0")/../../.." && pwd)"
image="${1:-docker.io/openwrt/rootfs:x86-64-openwrt-25.12}"
engine="$(command -v podman || command -v docker)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Build in a Linux container so apk-tools and root file ownership are known.
"$engine" run --rm -v "$here:/src:ro" -v "$work:/out" -w /src alpine:3.23 sh -ec '
  apk add --no-cache openssl
  mkdir /tmp/bins
  for b in cpxy-router cpxy-client cpxy-tsgw; do
    printf "#!/bin/sh\nexit 0\n" >/tmp/bins/$b
  done
  packaging/openwrt/build-apk.sh x86_64 1.0.0-r9 /tmp/bins /out
  packaging/openwrt/build-apk.sh x86_64 1.0.0-r10 /tmp/bins /out
  for dep in ip-full kmod-tun; do
    apk mkpkg --info name:$dep --info version:1-r0 --info arch:x86_64 --output /out/$dep.apk
  done
'
"$engine" run --rm -v "$work:/packages:ro" "$image" sh -ec '
  mkdir -p /var/lock /tmp
  # The rootfs image has not booted, so some configs (e.g. network) may not exist yet.
  snapshot() {
    for config in network dhcp firewall; do
      echo "== $config"
      if [ -e /etc/config/$config ]; then uci export $config; else echo absent; fi
    done
  }
  apk --no-network --repositories-file /dev/null --allow-untrusted add /packages/ip-full.apk /packages/kmod-tun.apk
  snapshot >/tmp/system.before
  apk --no-network --repositories-file /dev/null --allow-untrusted add /packages/cpxy-1.0.0-r9.apk
  for service in cpxy-client cpxy-router cpxy-tsgw; do
    /etc/init.d/$service enabled
  done
  # An upgrade must preserve both modified configuration and disabled services.
  /etc/init.d/cpxy-tsgw disable
  uci set cpxy.defaults.tsgw=0
  uci commit cpxy
  uci export cpxy >/tmp/cpxy.before
  apk --no-network --repositories-file /dev/null --allow-untrusted add /packages/cpxy-1.0.0-r10.apk
  apk info -e cpxy=1.0.0-r10
  uci export cpxy >/tmp/cpxy.after
  cmp /tmp/cpxy.before /tmp/cpxy.after
  if /etc/init.d/cpxy-tsgw enabled; then echo "FAIL: upgrade enabled disabled service"; exit 1; fi
  apk --no-network --repositories-file /dev/null del cpxy
  for service in cpxy-client cpxy-router cpxy-tsgw; do
    [ ! -e /etc/init.d/$service ]
    [ ! -e /usr/bin/$service ]
    for link in /etc/rc.d/*$service; do [ ! -e "$link" ]; done
  done
  [ ! -e /lib/apk/packages/cpxy.list ]
  [ -f /etc/config/cpxy ]
  snapshot >/tmp/system.after
  cmp /tmp/system.before /tmp/system.after
  echo "OpenWrt apk install, upgrade, enablement, config preservation and removal: passed"
'
