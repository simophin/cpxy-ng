#!/usr/bin/env python3
"""Bounded UDP destination-port diagnostic (no tunnel or key changes).

Server JSONL is private: it contains observed endpoints. Client JSON is sanitized.
Use source-restricted runtime firewall rules. A successful immediate control
alongside a missing cross-port reply is an observation, not proof of NAT filtering.
"""
import argparse
import json
import secrets
import select
import socket
import time


def serve(host, ports, seconds):
    sockets = []
    try:
        for port in ports:
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.bind((host, port))
            sockets.append(sock)
        print(json.dumps({'event': 'ready'}), flush=True)
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select(sockets, [], [], .1)
            for sock in ready:
                data, peer = sock.recvfrom(2048)
                try:
                    req = json.loads(data)
                    token = req['token']
                    if not isinstance(token, str) or len(token) != 32:
                        continue
                except (ValueError, KeyError, TypeError):
                    continue
                destination = sockets.index(sock)
                # Identical reply size/order across trials; each source replies
                # to the tuple actually observed for this destination.
                for source, other in enumerate(sockets):
                    reply = dict(token=token, destination=destination,
                                 source=source, observed_peer=list(peer))
                    other.sendto(json.dumps(reply).encode(), peer)
                    print(json.dumps(dict(event='sent', **reply)), flush=True)
    finally:
        for sock in sockets:
            sock.close()


def probe(host, ports, rounds, window, seed):
    import random
    rng = random.Random(seed)
    host = socket.gethostbyname(host)
    mappings = {}
    rows = []
    # Same socket across all rounds, including the initially uncontacted ports.
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(('0.0.0.0', 0))
        for trial in range(rounds):
            order = list(range(len(ports)))
            rng.shuffle(order)
            for destination in order:
                token = secrets.token_hex(16)
                start = time.monotonic()
                sock.sendto(json.dumps(dict(token=token)).encode(), (host, ports[destination]))
                replies = {}
                while time.monotonic() - start < window:
                    ready, _, _ = select.select([sock], [], [], max(0, window-(time.monotonic()-start)))
                    if not ready:
                        break
                    data, peer = sock.recvfrom(2048)
                    if peer[0] != host or peer[1] not in ports:
                        continue
                    try:
                        reply = json.loads(data)
                        source = reply['source']
                        observed = reply['observed_peer']
                        if (reply['token'] != token or reply['destination'] != destination
                                or type(source) is not int or source not in range(len(ports))
                                or ports[source] != peer[1] or not isinstance(observed, list)
                                or len(observed) != 2 or not isinstance(observed[0], str)
                                or type(observed[1]) is not int):
                            continue
                    except (ValueError, KeyError, TypeError):
                        continue
                    mapping = tuple(observed)
                    if mapping not in mappings:
                        mappings[mapping] = len(mappings)
                    replies[source] = dict(source=source, mapping=mappings[mapping],
                                           elapsed_ms=round((time.monotonic()-start)*1000, 3))
                rows.append(dict(round=trial, destination=destination,
                                 replies=list(replies.values())))
    return dict(schema=1, seed=seed, rounds=rounds, reply_window_seconds=window,
                port_count=len(ports), mapping_count=len(mappings), trials=rows,
                limitations=['No client-interface capture; absent socket delivery does not locate loss.',
                              'Immediate plain UDP probes do not establish tunnel idle retention or NAT timeout.',
                              'Cross-source probes can refresh filtering state; interpret first contact separately.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['server', 'client'])
    parser.add_argument('host')
    parser.add_argument('ports')
    parser.add_argument('--seconds', type=int, default=180)
    parser.add_argument('--rounds', type=int, default=10)
    parser.add_argument('--window', type=float, default=.5)
    parser.add_argument('--seed', type=int, default=1)
    args = parser.parse_args()
    try:
        ports = [int(v) for v in args.ports.split(',')]
    except ValueError:
        parser.error('ports must be integers')
    if not (2 <= len(ports) <= 8 and len(set(ports)) == len(ports)
            and all(1024 <= v <= 65535 for v in ports)
            and 1 <= args.seconds <= 300 and 1 <= args.rounds <= 30
            and .05 <= args.window <= 2):
        parser.error('probe bounds exceeded')
    if args.mode == 'server':
        serve(args.host, ports, args.seconds)
    else:
        print(json.dumps(probe(args.host, ports, args.rounds, args.window, args.seed), indent=2))


if __name__ == '__main__':
    main()
