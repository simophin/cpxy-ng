#!/usr/bin/env python3
"""Private, bounded UDP mapping probe. Run server/client with matching port list.
Server replies immediately with the observed tuple, then from every bound port
at the requested idle deadline. Client uses a fresh socket per deadline and
sends to every port once, then stays silent. Raw output contains endpoints.
Use only behind source-restricted temporary runtime firewall rules.
"""
import argparse
import heapq
import json
import secrets
import select
import socket
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('mode', choices=['server', 'client'])
p.add_argument('host')
p.add_argument('ports', help='comma-separated UDP ports')
p.add_argument('--delays', default='30,60,120,180')
p.add_argument('--seconds', type=int, default=240)
a = p.parse_args()
ports = [int(v) for v in a.ports.split(',')]
delays = [int(v) for v in a.delays.split(',')]
if not (1 <= len(ports) <= 8 and all(1 <= v <= 65535 for v in ports)
        and all(1 <= v <= 180 for v in delays) and 1 <= a.seconds <= 300):
    p.error('probe bounds exceeded')
sockets = []
queue = []
serial = 0
started = time.monotonic()
if a.mode == 'server':
    for port in ports:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.bind((a.host, port))
        sockets.append(s)
    while time.monotonic() - started < a.seconds:
        now = time.monotonic()
        while queue and queue[0][0] <= now:
            _, _, s, peer, data = heapq.heappop(queue)
            s.sendto(data, peer)
        ready, _, _ = select.select(sockets, [], [], .1)
        for s in ready:
            data, peer = s.recvfrom(1024)
            try:
                req = json.loads(data)
                delay, token = req['delay'], req['token']
                if delay not in delays or not isinstance(token, str) or len(token) != 32:
                    continue
            except (ValueError, KeyError, TypeError):
                continue
            reply = dict(token=token, delay=delay, destination_port=s.getsockname()[1],
                         observed_peer=peer, phase='immediate')
            s.sendto(json.dumps(reply).encode(), peer)
            # Bound memory even if the allowed source sends unexpected traffic.
            if len(queue) >= 1024:
                continue
            for other in sockets:
                serial += 1
                delayed = dict(reply, phase='delayed', reply_port=other.getsockname()[1])
                heapq.heappush(queue, (now + delay, serial, other, peer,
                                      json.dumps(delayed).encode()))
else:
    expected_host = socket.gethostbyname(a.host)
    rows = []
    for delay in delays:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.bind(('0.0.0.0', 0))
        sockets.append(s)
        token = secrets.token_hex(16)
        for port in ports:
            s.sendto(json.dumps(dict(token=token, delay=delay)).encode(), (expected_host, port))
        rows.append(dict(delay=delay, token=token, local_port=s.getsockname()[1], replies=[]))
    deadline = started + max(delays) + 8
    while time.monotonic() < deadline:
        ready, _, _ = select.select(sockets, [], [], .2)
        for s in ready:
            data, peer = s.recvfrom(1024)
            row = rows[sockets.index(s)]
            if peer[0] != expected_host or peer[1] not in ports:
                continue
            reply = json.loads(data)
            if reply.get('token') == row['token']:
                row['replies'].append(dict(reply, elapsed=round(time.monotonic()-started, 3)))
    print(json.dumps(rows, indent=2))
