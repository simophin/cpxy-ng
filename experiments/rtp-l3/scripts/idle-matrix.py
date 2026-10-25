#!/usr/bin/env python3
"""Bounded delayed UDP probes; raw JSONL contains private endpoints/tokens.

Each trial uses three distinct sockets: one silent delayed probe and immediate
controls at its start and deadline. Hold every socket until the run ends so no
local port is reused. No retries or early replies refresh the silent socket.
Use source-restricted temporary runtime firewall rules and explicit cleanup.
"""
import argparse
import hashlib
import heapq
import json
import math
import random
import secrets
import selectors
import socket
import time

PAYLOAD_BYTES = 192
MAX_TRIALS = 128


def wire(value):
    data = json.dumps(value, separators=(',', ':')).encode()
    if len(data) > PAYLOAD_BYTES:
        raise ValueError('probe payload too large')
    return data.ljust(PAYLOAD_BYTES, b' ')


def fingerprint(data):
    return hashlib.sha256(data).hexdigest()


def event(log, kind, **fields):
    log.write(json.dumps(dict(event=kind, wall_ns=time.time_ns(), **fields)) + '\n')
    log.flush()


def serve(host, ports, seconds, log):
    queue = []
    serial = 0
    with selectors.DefaultSelector() as selector:
        sockets = []
        try:
            for source, port in enumerate(ports):
                sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                sockets.append(sock)
                sock.bind((host, port))
                sock.setblocking(False)
                selector.register(sock, selectors.EVENT_READ, source)
            event(log, 'ready')
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                now = time.monotonic()
                # Bound the scheduler even for unexpected traffic from an
                # authorized source; each request schedules exactly one reply.
                for _ in range(256):
                    if not queue or queue[0][0] > now:
                        break
                    _, _, source, peer, reply, received = heapq.heappop(queue)
                    try:
                        sockets[source].sendto(reply, peer)
                        event(log, 'sent', token=json.loads(reply)['token'],
                              source=source, observed_peer=peer,
                              fingerprint=fingerprint(reply),
                              server_wait_ms=round((time.monotonic()-received)*1000, 3))
                    except OSError as error:
                        event(log, 'send_error', token=json.loads(reply)['token'],
                              errno=error.errno)
                timeout = min(.1, max(0, deadline-time.monotonic()))
                if queue:
                    timeout = min(timeout, max(0, queue[0][0]-time.monotonic()))
                for key, _ in selector.select(timeout):
                    data, peer = key.fileobj.recvfrom(2048)
                    received = time.monotonic()
                    try:
                        req = json.loads(data)
                        token, source, delay = req['token'], req['source'], req['delay']
                        if (len(data) != PAYLOAD_BYTES or not isinstance(token, str)
                                or len(token) != 32 or any(c not in '0123456789abcdef' for c in token)
                                or type(source) is not int or source not in range(len(ports))
                                or type(delay) not in (int, float) or not math.isfinite(delay)
                                or not 0 <= delay <= 180 or len(queue) >= 4096):
                            continue
                    except (ValueError, KeyError, TypeError):
                        continue
                    event(log, 'received', token=token, destination=key.data,
                          source=source, observed_peer=peer, delay_seconds=delay,
                          fingerprint=fingerprint(data))
                    reply = wire(dict(token=token, source=source, destination=key.data,
                                      observed_peer=peer))
                    serial += 1
                    heapq.heappush(queue, (received+delay, serial, source, peer, reply, received))
            event(log, 'stopped', pending_replies=len(queue))
        finally:
            for sock in sockets:
                sock.close()


def probe(host, ports, delays, rounds, window, seed, launch_gap, log):
    host = socket.gethostbyname(host)
    plan = [dict(round=r, destination=d, delay_seconds=delay, kind=kind)
            for r in range(rounds) for delay in delays for d in range(len(ports))
            for kind in ('same_source', 'cross_source')]
    random.Random(seed).shuffle(plan)
    queue = []
    sockets = []
    serial = 0
    started = time.monotonic()
    rng = random.Random(seed+1)
    for trial, row in enumerate(plan):
        row['trial'] = trial
        row['source'] = (row['destination'] if row['kind'] == 'same_source' else
                         rng.choice([s for s in range(len(ports)) if s != row['destination']]))
        row['legs'] = {}
        for leg, offset in (('delayed', 0), ('control_start', 0),
                            ('control_end', row['delay_seconds'])):
            serial += 1
            heapq.heappush(queue, (started+trial*launch_gap+offset, serial, row, leg))
    mappings = {}
    event(log, 'start', trials=len(plan), payload_bytes=PAYLOAD_BYTES)
    with selectors.DefaultSelector() as selector:
        try:
            while queue or selector.get_map():
                now = time.monotonic()
                while queue and queue[0][0] <= now:
                    _, _, row, leg = heapq.heappop(queue)
                    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                    sockets.append(sock)
                    sock.bind(('0.0.0.0', 0))
                    sock.setblocking(False)
                    delay = row['delay_seconds'] if leg == 'delayed' else 0
                    source = row['source'] if leg == 'delayed' else row['destination']
                    token = secrets.token_hex(16)
                    data = wire(dict(token=token, source=source, delay=delay))
                    sent = time.monotonic()
                    state = dict(row=row, leg=leg, token=token, source=source,
                                 sent=sent, expires=sent+delay+window)
                    row['legs'][leg] = dict(received=False, duplicates=0,
                                           send_offset_ms=round((sent-started)*1000, 3))
                    selector.register(sock, selectors.EVENT_READ, state)
                    sock.sendto(data, (host, ports[row['destination']]))
                    event(log, 'sent', trial=row['trial'], leg=leg, token=token,
                          local_port=sock.getsockname()[1], destination=row['destination'],
                          source=source, delay_seconds=delay, fingerprint=fingerprint(data))
                now = time.monotonic()
                for key in list(selector.get_map().values()):
                    if now >= key.data['expires']:
                        selector.unregister(key.fileobj)
                deadlines = [key.data['expires'] for key in selector.get_map().values()]
                if queue:
                    deadlines.append(queue[0][0])
                if not deadlines:
                    break
                for key, _ in selector.select(min(.1, max(0, min(deadlines)-time.monotonic()))):
                    data, peer = key.fileobj.recvfrom(2048)
                    state = key.data
                    row, leg = state['row'], state['leg']
                    try:
                        reply = json.loads(data)
                        observed = reply['observed_peer']
                        if (peer != (host, ports[state['source']]) or len(data) != PAYLOAD_BYTES
                                or reply['token'] != state['token']
                                or type(reply['source']) is not int or reply['source'] != state['source']
                                or type(reply['destination']) is not int
                                or reply['destination'] != row['destination']
                                or not isinstance(observed, list) or len(observed) != 2
                                or not isinstance(observed[0], str) or type(observed[1]) is not int
                                or not 1 <= observed[1] <= 65535):
                            continue
                        socket.inet_pton(socket.AF_INET, observed[0])
                    except (ValueError, KeyError, TypeError, OSError):
                        continue
                    mapping = tuple(observed)
                    mappings.setdefault(mapping, len(mappings))
                    result = row['legs'][leg]
                    event(log, 'received', trial=row['trial'], leg=leg, token=state['token'],
                          fingerprint=fingerprint(data), observed_peer=observed)
                    if result['received']:
                        result['duplicates'] += 1
                    else:
                        result.update(received=True, mapping=mappings[mapping],
                                      elapsed_ms=round((time.monotonic()-state['sent'])*1000, 3))
            event(log, 'complete')
        finally:
            for sock in sockets:
                sock.close()
    return dict(schema=1, seed=seed, rounds=rounds, delays_seconds=delays,
                reply_window_seconds=window, launch_gap_seconds=launch_gap,
                payload_bytes=PAYLOAD_BYTES, port_count=len(ports), mapping_count=len(mappings),
                duration_seconds=round(time.monotonic()-started, 3), trials=plan,
                limitations=[
                    'Plain fixed-size UDP is not authenticated tunnel traffic or Z4 acceptance.',
                    'Controls use separate sockets and cannot prove a silent mapping survived.',
                    'Successful send calls do not prove interface transmission or network delivery.',
                    'Missing socket delivery alone cannot distinguish filtering from network loss.',
                    'Each silent socket receives only one scheduled reply; no retries or immediate ACK.',
                    'Distinct local sockets do not prove distinct upstream state before first use.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['server', 'client'])
    parser.add_argument('host')
    parser.add_argument('ports')
    parser.add_argument('--seconds', type=float, default=240)
    parser.add_argument('--delays', default='5,30,60,120,180')
    parser.add_argument('--rounds', type=int, default=2)
    parser.add_argument('--window', type=float, default=2)
    parser.add_argument('--launch-gap', type=float, default=.05)
    parser.add_argument('--seed', type=int, default=25)
    parser.add_argument('--private-log', required=True, help='new private JSONL file (never commit)')
    args = parser.parse_args()
    try:
        ports = [int(v) for v in args.ports.split(',')]
        delays = [float(v) for v in args.delays.split(',')]
    except ValueError:
        parser.error('ports and delays must be numbers')
    trials = len(ports)*len(delays)*args.rounds*2
    if not (2 <= len(ports) <= 8 and len(set(ports)) == len(ports)
            and all(1024 <= v <= 65535 for v in ports)
            and 1 <= len(delays) <= 8 and len(set(delays)) == len(delays)
            and all(math.isfinite(v) and .05 <= v <= 180 for v in delays)
            and 1 <= args.rounds <= 5 and trials <= MAX_TRIALS
            and .05 <= args.window <= 5 and 0 <= args.launch_gap <= .2
            and 1 <= args.seconds <= 300):
        parser.error('probe bounds exceeded')
    # Exclusive create avoids clobbering evidence or following a symlink.
    import os
    with os.fdopen(os.open(args.private_log, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as log:
        if args.mode == 'server':
            serve(args.host, ports, args.seconds, log)
        else:
            print(json.dumps(probe(args.host, ports, delays, args.rounds, args.window,
                                   args.seed, args.launch_gap, log), indent=2))


if __name__ == '__main__':
    main()
