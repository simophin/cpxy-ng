#!/usr/bin/env python3
"""Open host UDP socket, then inherit it into unprivileged TUN/app namespace.
Usage: client-netns.py BINARY KEY_FILE ENDPOINT:PORT IFACE CIDR RUN_SCRIPT [BIND_PORT]
BIND_PORT fixes only the initial bootstrap port; candidate/recovery sockets use
fresh host-network ports. PING learns each registered NAT mapping.
RUN_SCRIPT runs inside the namespace after TUN is ready. Paths are supplied by
operator; never embed deployment endpoints or keys in this file.
"""
import os
import array
import select
import socket
import subprocess
import sys

if not 7 <= len(sys.argv) <= 8:
    sys.exit(__doc__)
binary, key, endpoint, iface, cidr, runscript = sys.argv[1:7]
host, port = endpoint.rsplit(":", 1)
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
if len(sys.argv) > 7:
    sock.bind(("0.0.0.0", int(sys.argv[7])))
sock.connect((host, int(port)))
os.set_inheritable(sock.fileno(), True)
env = dict(os.environ, POC_BINARY=binary, POC_KEY=key, POC_IFACE=iface,
           POC_CIDR=cidr, POC_FD=str(sock.fileno()), POC_RUN=runscript)
# Keep a private broker in host networking. Fresh UDP sockets cross the
# namespace boundary via SCM_RIGHTS; keys and tunnel packets never use it.
broker, inherited = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
os.set_inheritable(inherited.fileno(), True)
env['POC_SOCKET_BROKER_FD'] = str(inherited.fileno())
# Shell owns tunnel lifetime and always waits after TERM so final counters flush.
script = '''
"$POC_BINARY" client "$POC_KEY" "$POC_IFACE" "$POC_CIDR" "$POC_FD" &
poc_pid=$!
export POC_TUN_PID="$poc_pid"
trap 'kill -TERM "$poc_pid" 2>/dev/null; wait "$poc_pid"' EXIT INT TERM
poc_wait=0
until ip -o -4 addr show dev "$POC_IFACE" 2>/dev/null | grep -Fq " $POC_CIDR "; do
  kill -0 "$poc_pid" 2>/dev/null || exit 1
  poc_wait=$((poc_wait+1)); [ "$poc_wait" -lt 100 ] || exit 1
  sleep 0.1
done
until ip -o link show up dev "$POC_IFACE" 2>/dev/null | grep -q .; do
  kill -0 "$poc_pid" 2>/dev/null || exit 1
  poc_wait=$((poc_wait+1)); [ "$poc_wait" -lt 900 ] || exit 1
  sleep 0.1
done
sh "$POC_RUN"
'''
child = subprocess.Popen(["unshare", "-Urn", "sh", "-c", script],
                         pass_fds=(sock.fileno(), inherited.fileno()), env=env)
inherited.close()
sock.close()
try:
    while child.poll() is None:
        readable, _, _ = select.select([broker], [], [], .2)
        if readable:
            request = broker.recv(16)
            if not request:
                break
            if len(request) != 2:
                raise RuntimeError('invalid socket broker request')
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as fresh:
                fresh.bind(('0.0.0.0', 0))
                broker.sendmsg([request], [(socket.SOL_SOCKET, socket.SCM_RIGHTS,
                                           array.array('i', [fresh.fileno()]))])
    sys.exit(child.wait())
finally:
    broker.close()
    if child.poll() is None:
        child.terminate()
        child.wait()
