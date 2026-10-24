#!/usr/bin/env python3
"""Loopback-only deterministic bounded UDP fault injector for integration tests.
Drops first CHALLENGE/READY, then periodic data loss/duplicates/reorder/tampering.
"""
import selectors
import socket
import sys

listen, upstream = map(int, sys.argv[1:])
a = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
a.bind(("127.0.0.1", listen))
b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
b.connect(("127.0.0.1", upstream))
sel = selectors.DefaultSelector()
sel.register(a, selectors.EVENT_READ)
sel.register(b, selectors.EVENT_READ)
client = None
seen_control = set()
counts = [0, 0]
held = [None, None]
while True:
    for key, _ in sel.select(0.05):
        d, source = key.fileobj.recvfrom(65536)
        direction = int(key.fileobj is b)
        if not direction:
            client = source
        if client is None:
            continue
        target, address = (a, client) if direction else (b, None)
        def send(payload):
            target.sendto(payload, address) if address else target.send(payload)
        if d.startswith(b"R3P1"):
            kind = d[4]
            if kind in (2, 4) and kind not in seen_control:
                seen_control.add(kind)
                print("dropped control", kind, flush=True)
                continue
            send(d)
            continue
        counts[direction] += 1
        n = counts[direction]
        if n % 73 == 0:
            continue
        if n % 113 == 0:
            d = d[:-1] + bytes([d[-1] ^ 1])
        if held[direction] is not None:
            send(d)
            send(held[direction])
            held[direction] = None
            continue
        if n % 41 == 0:
            held[direction] = d
            continue
        send(d)
        if n % 37 == 0:
            send(d)
