#!/usr/bin/env python3
"""Loopback-only deterministic bounded UDP fault injector for integration tests.
Periodic data loss/duplicates/reorder/tampering in both directions.
Usage: fault-proxy.py BASE_PORT ROTATE_LOW ROTATE_HIGH
"""
import selectors
import socket
import sys
import time

# One forwarding pair per server port. STATE remains encrypted and names the
# real port; use a second loopback IP so the proxy can expose identical ports.
# Usage: fault-proxy.py BASE_PORT ROTATE_LOW ROTATE_HIGH
base, low, high = map(int, sys.argv[1:4])
sel = selectors.DefaultSelector()
for port in [base, *range(low, high + 1)]:
    front = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    front.bind(("127.0.0.2", port))
    back = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    back.connect(("127.0.0.1", port))
    pair = {"front": front, "back": back, "client": None}
    sel.register(front, selectors.EVENT_READ, (pair, 0))
    sel.register(back, selectors.EVENT_READ, (pair, 1))
counts = [0, 0]
held = [None, None]
held_at = [0.0, 0.0]
targets = [None, None]
while True:
    # Bound reordering: a held packet is released after 20 ms even when no
    # later packet follows, so quiet control connections cannot stall.
    for direction in (0, 1):
        if held[direction] is not None and time.monotonic() - held_at[direction] > 0.02:
            targets[direction](held[direction])
            held[direction] = None
    for key, _ in sel.select(0.01):
        pair, direction = key.data
        try:
            d, source = key.fileobj.recvfrom(65536)
        except ConnectionRefusedError:
            continue  # A rotation socket may have retired.
        if not direction:
            pair["client"] = source
        if pair["client"] is None:
            continue
        target, address = (pair["front"], pair["client"]) if direction else (pair["back"], None)
        def send(payload, target=target, address=address):
            target.sendto(payload, address) if address else target.send(payload)
        targets[direction] = send
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
            held_at[direction] = time.monotonic()
            continue
        send(d)
        if n % 37 == 0:
            send(d)
