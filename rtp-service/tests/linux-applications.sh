#!/bin/bash
set -euo pipefail
if [ "$(readlink /proc/self/ns/net)" = "$(readlink /proc/1/ns/net)" ]; then echo 'Run inside unshare -Urnm' >&2; exit 1; fi
mount --make-rprivate /
mount -t tmpfs tmpfs /run
for n in c e b; do ip netns add "$n"; ip -n "$n" link set lo up; done
ip link add vc type veth peer name ve
ip link set vc netns c
ip link set ve netns e
ip -n c addr add 192.0.2.1/24 dev vc
ip -n e addr add 192.0.2.2/24 dev ve
ip -n c link set vc up
ip -n e link set ve up
ip link add eb type veth peer name be
ip link set eb netns e
ip link set be netns b
ip -n e addr add 10.90.0.1/24 dev eb
ip -n b addr add 10.90.0.2/24 dev be
ip -n e link set eb up
ip -n b link set be up
# Explicit deployment policy, never installed by the service.
ip netns exec e python3 -c 'open("/proc/sys/net/ipv4/ip_forward","w").write("1")'
ip -n b route add 10.88.0.0/24 via 10.90.0.1
# Deliberately working physical fallback: only policy rules must prevent leaks.
ip -n c route add default via 192.0.2.2
ip -n e route add 192.0.2.0/24 dev ve 2>/dev/null || true
work=$(mktemp -d /tmp/rtp-a3-XXXX)
export CPXY_RTP_PASSWORD="fixture-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
export CPXY_RTP_KEEPALIVE_SECONDS=5 CPXY_RTP_SERVICE_CIDRS=10.90.0.0/24
server_env=(env CPXY_RTP_STATE_DIR="$work/server" CPXY_RTP_INTERFACE=rtpe CPXY_RTP_LISTEN=192.0.2.2:30000
  CPXY_RTP_ROTATION_PORTS=30001-30008 CPXY_RTP_ROTATION_SECONDS=0 CPXY_RTP_FIREWALL=none)
client_env=(env CPXY_RTP_STATE_DIR="$work/client" CPXY_RTP_INTERFACE=rtpc CPXY_RTP_OUTBOUND_INTERFACE=vc
  CPXY_RTP_ENTRY=192.0.2.2:30000 CPXY_RTP_BIND=192.0.2.1:0)
cat > "$work/backend.py" <<'PY'
import socket,threading

def tcp():
 s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('10.90.0.2',8080));s.listen()
 while True:
  c,a=s.accept();d=c.recv(100);c.sendall(b'tcp:'+d);c.close()
t=threading.Thread(target=tcp,daemon=True);t.start()
u=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);u.bind(('10.90.0.2',8080))
while True:
 d,a=u.recvfrom(100);u.sendto(b'udp:'+d,a)
PY
cat > "$work/app.py" <<'PY'
import socket
for kind,prefix in [(socket.SOCK_STREAM,b'tcp:'),(socket.SOCK_DGRAM,b'udp:')]:
 s=socket.socket(socket.AF_INET,kind);s.settimeout(2);s.connect(('10.90.0.2',8080));s.send(b'application');assert s.recv(100)==prefix+b'application';s.close()
PY
bin=$(realpath "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}")
server=''; client=''; backend=''
cleanup(){ for pid in "$server" "$client" "$backend";do if [ -n "$pid" ];then kill "$pid" 2>/dev/null || true;wait "$pid" 2>/dev/null || true;fi;done; }
trap cleanup EXIT
ip netns exec b python3 "$work/backend.py" > "$work/backend.log" 2>&1 & backend=$!
ip netns exec e "${server_env[@]}" "$bin" endpoint > "$work/e.out" 2> "$work/e.log" & server=$!
rtpc(){ ip netns exec c "${client_env[@]}" "$bin" client "$@"; }
# Backgrounding a function would fork a subshell; signal the service itself.
start_client(){ ip netns exec c "${client_env[@]}" "$bin" client >> "$work/c.out" 2>> "$work/c.log" & client=$!; sleep 2; }
start_client
ip netns exec c python3 "$work/app.py"
ip netns exec c ip route get 10.90.0.2 | grep -Eq 'dev rtpc.*table 18800'
# An unrelated admin route/rule must survive all service operations.
ip -n c route add 198.51.100.0/24 via 192.0.2.2
ip -n c rule add priority 2000 to 198.51.100.0/24 table main
kill -KILL "$client"; wait "$client" 2>/dev/null || true; client=''
if ip -n c link show rtpc 2>/dev/null; then exit 1; fi
rtpc cleanup-ports
if ip netns exec c ip route get 10.90.0.2 > "$work/crash-route.log" 2>&1; then echo 'crash exposed fallback'; exit 1; fi
if ip netns exec c python3 "$work/app.py" > "$work/crash-app.log" 2>&1; then echo 'crash leaked app'; exit 1; fi
start_client
ip netns exec c python3 "$work/app.py"
# SIGHUP is a graceful stop; a fresh start reuses the profile.
kill -HUP "$client"; wait "$client"
start_client
ip netns exec c python3 "$work/app.py"
# Transport outage keeps guard/routes in place; applications cannot use underlay.
kill -STOP "$server"
if ip netns exec c python3 "$work/app.py" > "$work/outage-app.log" 2>&1; then exit 1; fi
kill -CONT "$server"
sleep 1
ip netns exec c python3 "$work/app.py"
kill -TERM "$client"; wait "$client"; client=''
ip -n c rule show | grep -Eq '2000:.*198.51.100.0/24'
ip -n c route show | grep -Eq '198.51.100.0/24'
[ "$(ip -n c -j route show table 18800)" = '[]' ]
if ip -n c rule show | grep -Eq '1000:';then exit 1;fi
# Failed startup refuses a preexisting administrator interface. Guard remains
# until explicit cleanup; that cleanup must preserve the unrelated interface.
ip -n c link add rtpc type dummy
if rtpc > "$work/startup.out" 2> "$work/startup.log";then exit 1;fi
ip -n c link show rtpc >/dev/null
if ip netns exec c ip route get 10.90.0.2;then exit 1;fi
rtpc cleanup-ports
if ip netns exec c ip route get 10.90.0.2;then exit 1;fi
rtpc cleanup
rtpc cleanup
ip -n c link show rtpc >/dev/null
ip -n c link del rtpc
# DNS is external policy metadata only: a resolver needs suffixes, and must lie
# inside a selected service CIDR. Both fail before touching any configuration.
if rtpc --dns-resolver 10.90.0.53 > "$work/dns.out" 2> "$work/dns.log";then exit 1;fi
if rtpc --dns-resolver 10.91.0.53 --dns-suffix corp.example > "$work/dns.out" 2>> "$work/dns.log";then exit 1;fi
kill -TERM "$server";wait "$server";server=''
[ "$(ip -n e -j route show table 18800)" = '[]' ]
echo "A3_APPLICATIONS_OK artifacts=$work"
