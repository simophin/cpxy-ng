#!/bin/sh
# Checks the dns instance's wrapper (/usr/libexec/cpxy/dns-split.sh) in an OpenWrt 24.10 container:
# dnsmasq is pointed at the resolver only once it listens, and handed back when it exits or is
# stopped. dnsmasq itself stands in for dns_split; the dnsmasq service restarts are recorded only.
#
# usage: dns-handover.sh <ipk> [image]    (needs podman or docker)
set -eu

ipk="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
image="${2:-docker.io/openwrt/rootfs:x86-64-openwrt-24.10}"
engine="$(command -v podman || command -v docker)"

script='
set -u
mkdir -p /var/lock /tmp
for dep in ip-full kmod-tun; do
	printf "Package: %s\nVersion: 1\nStatus: install ok installed\nArchitecture: x86_64\n\n" "$dep" >>/usr/lib/opkg/status
	: >"/usr/lib/opkg/info/$dep.list"
done
opkg install /pkg.ipk >/dev/null 2>&1 || { echo "FAIL: install"; exit 1; }

printf "#!/bin/sh\necho \"\$*\" >>/tmp/dnsmasq-calls\n" >/etc/init.d/dnsmasq
. /usr/libexec/cpxy/dnsmasq.sh
dropin="$(_cpxy_dnsmasq_confdir)/cpxy.conf"
W=/usr/libexec/cpxy/dns-split.sh
# Word-split on purpose when used
RESOLVER="dnsmasq -k -C /dev/null -u root --pid-file= -p 5335 --listen-address=127.0.0.1 --bind-interfaces"
wait_dropin() { i=0; while [ ! -e "$dropin" ] && [ $i -lt 10 ]; do sleep 1; i=$((i + 1)); done; [ -e "$dropin" ]; }
fail() { echo "FAIL: $*"; exit 1; }

# A resolver that never comes up never gets the LAN DNS, and its exit status reaches procd
"$W" 127.0.0.1:5335 sh -c "sleep 2; exit 3"
[ $? = 3 ] || fail "exit status not passed on"
[ ! -e "$dropin" ] || fail "dnsmasq pointed at a resolver that did not start"
[ ! -s /tmp/dnsmasq-calls ] || fail "dnsmasq restarted for a resolver that did not start"
echo "ok - no handover to a resolver that fails to start"

# Handed over once it listens, handed back when it dies
"$W" 127.0.0.1:5335 $RESOLVER &
w=$!
wait_dropin || fail "dnsmasq not pointed at a listening resolver"
grep -qxF "server=127.0.0.1#5335" "$dropin" || fail "drop-in content: $(cat "$dropin")"
kill "$(pidof dnsmasq)"
wait "$w"
[ ! -e "$dropin" ] || fail "drop-in left behind after the resolver died"
echo "ok - handed over while listening, back when it dies"

# procd stopping the instance (SIGTERM to the wrapper) stops the resolver and hands DNS back
"$W" 127.0.0.1:5335 $RESOLVER &
w=$!
wait_dropin || fail "dnsmasq not pointed at a listening resolver"
kill -TERM "$w"
wait "$w"
[ ! -e "$dropin" ] || fail "drop-in left behind after SIGTERM"
sleep 1
[ -z "$(pidof dnsmasq)" ] || fail "resolver still running after SIGTERM"
echo "ok - SIGTERM stops the resolver and hands DNS back"
echo "dns handover ok"
'

"$engine" run --rm -v "$ipk:/pkg.ipk:ro" "$image" sh -c "$script"
