#!/usr/bin/env python3
"""Open host UDP socket, then inherit it into unprivileged TUN/app namespace.
Usage: client-netns.py BINARY KEY_FILE ENDPOINT:PORT IFACE CIDR RUN_SCRIPT
RUN_SCRIPT runs inside the namespace after TUN is ready. Paths are supplied by
operator; never embed deployment endpoints or keys in this file.
"""
import os
import socket
import subprocess
import sys

if len(sys.argv) != 7:
    sys.exit(__doc__)
binary, key, endpoint, iface, cidr, runscript = sys.argv[1:]
host, port = endpoint.rsplit(":", 1)
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.connect((host, int(port)))
os.set_inheritable(sock.fileno(), True)
env = dict(os.environ, POC_BINARY=binary, POC_KEY=key, POC_IFACE=iface,
           POC_CIDR=cidr, POC_FD=str(sock.fileno()), POC_RUN=runscript)
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
  sleep 0.1
done
sh "$POC_RUN"
'''
sys.exit(subprocess.call(["unshare", "-Urn", "sh", "-c", script],
                         pass_fds=(sock.fileno(),), env=env))
