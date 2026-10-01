#!/bin/sh
# Runs the init script's start_service() in an OpenWrt 24.10 container with procd, netifd and the
# routing helpers stubbed out, and checks the commands it would have run, using the real UCI
# config parsing. Catches argument and config-handling mistakes; it starts nothing.
#
# usage: init-dryrun.sh <ipk> [image]    (needs podman or docker)
set -eu

ipk="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
image="${2:-docker.io/openwrt/rootfs:x86-64-openwrt-24.10}"
engine="$(command -v podman || command -v docker)"

script='
set -e
mkdir -p /var/lock /tmp
opkg update >/dev/null
opkg install ip-full kmod-tun >/dev/null 2>&1 || true
opkg install /pkg.ipk >/dev/null 2>&1

uci set cpxy.main.enabled=1
uci set cpxy.main.server="https://:s3cret@proxy.example:443"
uci add_list cpxy.main.dns_server=1.1.1.1
uci add_list cpxy.main.dns_upstream=9.9.9.9
uci commit cpxy

LOG=/tmp/calls.txt; : >"$LOG"
# Stubs: record what the init script asks of procd, the network and the helpers
procd_open_instance() { echo "instance $1" >>"$LOG"; }
procd_close_instance() { :; }
procd_set_param() { echo "set $*" >>"$LOG"; }
procd_append_param() { echo "append $*" >>"$LOG"; }

. /lib/functions.sh
. /etc/init.d/cpxy
# The init script sources the real network helpers; stub them afterwards
network_get_device() { eval "$1=br-lan"; }
logger() { :; }
cpxy_net_up() { echo "net_up $*" >>"$LOG"; }
cpxy_dnsmasq_up() { echo "dnsmasq_up $*" >>"$LOG"; }
start_service

cat "$LOG"
want() { grep -qF -- "$1" "$LOG" || { echo "FAIL: expected: $1"; exit 1; }; }
want "instance proxy"
want "set command /usr/bin/cpxy-client --socks5-proxy-listen 127.0.0.1:1080 --api-listen 127.0.0.1:3010"
want "append command --dns-server 223.5.5.5"
want "append command --dns-server 1.1.1.1"
want "set env SERVER=https://:s3cret@proxy.example:443 NO_COLOR=1"
want "instance tun"
want "set command /usr/bin/cpxy-tun2proxy --proxy socks5://127.0.0.1:1080 --tun cpxy0 --dns direct --exit-on-fatal-error"
want "net_up br-lan"
want "instance dns"
want "set command /usr/bin/cpxy-dns-split --listen 127.0.0.1:5353"
want "append command --upstream 223.5.5.5"
want "append command --upstream 9.9.9.9"
want "append command --alternative https://1.1.1.1/dns-query"
want "append command --cache-db /tmp/cpxy/dns-cache.sqlite"
want "dnsmasq_up 127.0.0.1:5353"
# The key must only travel by environment
grep -F "s3cret" "$LOG" | grep -vF "set env SERVER=" && { echo "FAIL: the key appears outside the environment"; exit 1; }
echo "dry run ok"
'

"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
