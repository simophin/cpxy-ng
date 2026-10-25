#!/usr/bin/env python3
"""Join private idle-matrix JSONL and optional client/server classic pcap.

Print only numbered trials, mappings and event presence. Raw event logs and
captures must stay private. Fingerprints match exact UDP payloads; interface
observation does not prove downstream delivery. Capture-drop counts are supplied
by the operator from the capture tool's final statistics.
"""
import argparse
import collections
import hashlib
import json
import struct


def capture_counts(path, wanted):
    counts = collections.Counter()
    if path is None:
        return None
    with open(path, 'rb') as stream:
        header = stream.read(24)
        if len(header) != 24 or header[:4] not in (
                b'\xd4\xc3\xb2\xa1', b'\xa1\xb2\xc3\xd4',
                b'\x4d\x3c\xb2\xa1', b'\xa1\xb2\x3c\x4d'):
            raise ValueError('classic pcap required')
        endian = '<' if header[:4] in (b'\xd4\xc3\xb2\xa1', b'\x4d\x3c\xb2\xa1') else '>'
        link = struct.unpack(endian+'I', header[20:24])[0]
        if link not in (1, 101, 113, 276):
            raise ValueError('Ethernet, raw IP or Linux cooked pcap required')
        while True:
            record = stream.read(16)
            if not record:
                break
            if len(record) != 16:
                raise ValueError('truncated pcap record')
            length = struct.unpack(endian+'IIII', record)[2]
            if length > 1024*1024:
                raise ValueError('oversized pcap record')
            packet = stream.read(length)
            if len(packet) != length:
                raise ValueError('truncated pcap packet')
            if link == 1:
                offset, protocol = 14, packet[12:14]
                while protocol in (b'\x81\x00', b'\x88\xa8') and len(packet) >= offset+4:
                    protocol = packet[offset+2:offset+4]
                    offset += 4
            elif link == 113:
                offset, protocol = 16, packet[14:16]
            elif link == 276:
                offset, protocol = 20, packet[:2]
            else:
                offset, protocol = 0, b'\x08\x00'
            if protocol != b'\x08\x00':
                continue
            ip = packet[offset:]
            if len(ip) < 20 or ip[0] >> 4 != 4 or ip[9] != 17:
                continue
            ihl = (ip[0] & 15)*4
            total = struct.unpack('!H', ip[2:4])[0]
            if (ihl < 20 or total < ihl+8 or len(ip) < total
                    or struct.unpack('!H', ip[6:8])[0] & 0x3fff):
                continue
            udp_length = struct.unpack('!H', ip[ihl+4:ihl+6])[0]
            if udp_length < 8 or ihl+udp_length > total:
                continue
            digest = hashlib.sha256(ip[ihl+8:ihl+udp_length]).hexdigest()
            if digest in wanted:
                counts[digest] += 1
    return counts


def analyze(client_path, server_path, client_pcap=None, server_pcap=None):
    def read(path):
        with open(path) as stream:
            return [json.loads(line) for line in stream if line.strip()]
    client, server = read(client_path), read(server_path)
    transmissions = [v for v in client if v['event'] == 'sent']
    server_rx = {v['token']: v for v in server if v['event'] == 'received'}
    server_tx = {v['token']: v for v in server if v['event'] == 'sent'}
    client_rx = {v['token']: v for v in client if v['event'] == 'received'}
    socket_rx = collections.Counter(v['token'] for v in client if v['event'] == 'socket_received')
    accepted_rx = collections.Counter(v['token'] for v in client if v['event'] == 'received')
    all_receives = any(v.get('records_all_socket_receives') for v in client if v['event'] == 'start')
    wanted = {v['fingerprint'] for v in transmissions+list(server_tx.values())}
    captures = dict(client=capture_counts(client_pcap, wanted),
                    server=capture_counts(server_pcap, wanted))
    mappings = {}
    rows = []
    for sent in transmissions:
        token = sent['token']
        rx, tx = server_rx.get(token), server_tx.get(token)
        row = dict(trial=sent['trial'], leg=sent['leg'], destination=sent['destination'],
                   source=sent['source'], delay_seconds=sent['delay_seconds'],
                   server_received=rx is not None, server_sent=tx is not None,
                   client_received=token in client_rx,
                   client_socket_packets=socket_rx[token] if all_receives else None,
                   client_rejected_packets=socket_rx[token]-accepted_rx[token] if all_receives else None)
        if rx:
            mapping = tuple(rx['observed_peer'])
            mappings.setdefault(mapping, len(mappings))
            row['mapping'] = mappings[mapping]
        if tx:
            row['server_wait_ms'] = tx['server_wait_ms']
        for side, counts in captures.items():
            row[side+'_capture_request_packets'] = None if counts is None else counts[sent['fingerprint']]
            row[side+'_capture_reply_packets'] = (None if counts is None or tx is None
                                                  else counts[tx['fingerprint']])
        rows.append(row)
    return dict(schema=1, mapping_count=len(mappings), legs=rows,
                client_completed=any(v['event'] == 'complete' for v in client),
                server_stopped=any(v['event'] == 'stopped' for v in server),
                server_pending_replies=[v['pending_replies'] for v in server if v['event'] == 'stopped'],
                limitations=[
                    'Send calls and passive captures do not prove remote delivery.',
                    'Absent packets can reflect capture coverage/drops, filtering or ordinary loss.',
                    'No client capture means return-path loss cannot be located at the client interface.',
                    'These plain UDP probes cannot establish tunnel retention or an exact NAT timeout.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('client_log')
    parser.add_argument('server_log')
    parser.add_argument('--client-pcap')
    parser.add_argument('--server-pcap')
    parser.add_argument('--client-capture-drops', type=int)
    parser.add_argument('--server-capture-drops', type=int)
    args = parser.parse_args()
    for side in ('client', 'server'):
        drops = getattr(args, side+'_capture_drops')
        if drops is not None and (drops < 0 or getattr(args, side+'_pcap') is None):
            parser.error('capture drop counts require a capture and must be nonnegative')
    result = analyze(args.client_log, args.server_log, args.client_pcap, args.server_pcap)
    result['capture_drops'] = dict(client=args.client_capture_drops, server=args.server_capture_drops)
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
