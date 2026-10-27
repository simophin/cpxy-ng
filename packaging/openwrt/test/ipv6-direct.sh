#!/usr/bin/env bash
# Run as root in private network AND mount namespaces (no host mutations):
# RTP_ROUTER_BIN=/path/cpxy-rtp-router unshare -nm bash packaging/openwrt/test/ipv6-direct.sh
set -euo pipefail
[[ $(id -u) = 0 && $(readlink /proc/self/ns/net) != $(readlink /proc/1/ns/net) && $(readlink /proc/self/ns/mnt) != $(readlink /proc/1/ns/mnt) ]] || { echo 'Private root network/mount namespaces required' >&2; exit 1; }
root=$(cd "$(dirname "$0")/../../.." && pwd)
bin=${RTP_ROUTER_BIN:-$root/target/debug/cpxy-rtp-router}
work=$(mktemp -d /tmp/rtp-direct6-XXXX)
server=
cleanup() { [[ -z "$server" ]] || kill "$server" 2>/dev/null || :; [[ -z "$server" ]] || wait "$server" 2>/dev/null || :; ip netns del c 2>/dev/null || :; ip netns del b 2>/dev/null || :; rm -rf "$work"; }
trap cleanup EXIT
mount --make-rprivate /
mount -t tmpfs tmpfs /run
ip link set lo up
for n in c b; do ip netns add "$n"; ip -n "$n" link set lo up; done
for pair in 'br-hybrid cl c' 'wan be b'; do
 read -r here there ns <<<"$pair"
 ip link add "$here" type veth peer name "$there"
 ip link set "$there" netns "$ns"
 ip link set "$here" up; ip -n "$ns" link set "$there" up
done
ip -6 addr add 2001:db8:30::1/64 dev br-hybrid nodad
ip -n c -6 addr add 2001:db8:30::2/64 dev cl nodad
ip -n c -6 route add default via 2001:db8:30::1
ip -6 addr add 2001:db8:1::1/64 dev wan nodad
ip -n b -6 addr add 2001:db8:1::2/64 dev be nodad
ip -n b -6 addr add 2400:3200::1/128 dev lo nodad
ip -n b -6 addr add 2606:4700:4700::1111/128 dev lo nodad
ip -6 route add default via 2001:db8:1::2
ip -n b -6 route add 2001:db8:30::/64 via 2001:db8:1::1
sysctl -qw net.ipv6.conf.all.forwarding=1
mkdir "$work/state"
echo br-hybrid >"$work/state/devices"
echo 223.5.5.5/32 >"$work/state/routes"
"$bin" --export-direct-routes6 >"$work/state/routes6"
sed "s|^P=.*|P=$work/state|" "$root/packaging/openwrt/rtp-files/usr/libexec/cpxy-rtp/guard.sh" >"$work/guard.sh"
# Exercise removal of the old terminal IPv6 prohibition only after the nft guard is in place.
ip -6 rule add priority 12004 iif br-hybrid prohibit
bash "$work/guard.sh"
P="$work/state" PRIO=12000 devices=br-hybrid
eval "$(sed -n '/^guard() {/,/^}/p' "$root/packaging/openwrt/rtp-files/usr/libexec/cpxy-rtp/policy.sh")"
guard
[[ -z $(ip -6 rule show priority 12004) ]]
cat >"$work/server.py" <<'PY'
import socket,threading,time
for address in ['2400:3200::1','2606:4700:4700::1111']:
    def tcp(address=address):
        s=socket.socket(socket.AF_INET6,socket.SOCK_STREAM); s.bind((address,8080)); s.listen()
        while True:
            c,a=s.accept(); c.recv(100); c.sendall(b'ok'); c.close()
    def udp(address=address):
        s=socket.socket(socket.AF_INET6,socket.SOCK_DGRAM); s.bind((address,8080))
        while True:
            d,a=s.recvfrom(100); s.sendto(d,a)
    threading.Thread(target=tcp,daemon=True).start(); threading.Thread(target=udp,daemon=True).start()
time.sleep(.1); print('READY',flush=True)
while True: time.sleep(1)
PY
ip netns exec b python3 "$work/server.py" >"$work/server.log" 2>&1 & server=$!
for _ in $(seq 1 50); do grep -q READY "$work/server.log" && break; sleep .1; done
grep -q READY "$work/server.log"
cat >"$work/client.py" <<'PY'
import errno,socket,sys,time
address,mode=sys.argv[1:]
for proto in [socket.SOCK_STREAM,socket.SOCK_DGRAM]:
    with socket.socket(socket.AF_INET6,proto) as s:
        s.settimeout(2); start=time.monotonic()
        try:
            s.connect((address,8080)); s.send(b'ok'); assert s.recv(100)==b'ok'
            assert mode=='allow','foreign IPv6 escaped'
        except OSError as e:
            assert mode in ('reject','blocked'),(address,e)
            assert e.errno in (errno.ECONNREFUSED,errno.EACCES,errno.EHOSTUNREACH,errno.ENETUNREACH),e
            if mode=='reject':
                assert time.monotonic()-start<1,'rejection took too long'
                if proto==socket.SOCK_STREAM: assert e.errno==errno.ECONNREFUSED,'expected TCP reset'
print(address,mode,'TCP+UDP OK')
PY
# Router-originated transport is outside the selected-LAN restriction.
python3 "$work/client.py" 2606:4700:4700::1111 allow
ip netns exec c python3 "$work/client.py" 2400:3200::1 allow
ip netns exec c python3 "$work/client.py" 2606:4700:4700::1111 reject
ip netns exec c ping -6 -n -c1 -W1 2001:db8:30::1 >/dev/null
# Independent guard reinstatement (as during firewall reload) keeps the same policy.
bash "$work/guard.sh"
ip netns exec c python3 "$work/client.py" 2400:3200::1 allow
ip netns exec c python3 "$work/client.py" 2606:4700:4700::1111 reject
# Disabling the option restores the original terminal IPv6 guard.
rm "$work/state/routes6"
bash "$work/guard.sh"
guard
ip -6 rule show priority 12004 | grep -q prohibit
ip netns exec c python3 "$work/client.py" 2400:3200::1 blocked
echo IPV6_DIRECT_GUARD_OK
