#!/usr/bin/env python3
"""Correlate POC_TRACE_CONTROL JSONL from matching client/server runs.
Usage: control-trace.py CLIENT_JSONL SERVER_JSONL [SERVER_ETHERNET_PCAP]
Emits endpoint-free per-port aggregates and incomplete PING/STATE sequences.
Missing means no matching runtime receive event, not proven wire/path loss.
Raw input stays private. Clock bounds assume nonnegative packet transit times;
no synchronized-clock assumption is required for fingerprint matching.
"""
import collections
import json
import hashlib
import struct
import sys


def load(path):
    rows = []
    with open(path) as source:
        for line in source:
            if line.startswith('{'):
                row = json.loads(line)
                if row.get('event') == 'control_trace':
                    rows.append(row)
    return rows


def capture_fingerprints(path):
    """Fingerprint complete, unfragmented IPv4 UDP payloads from Ethernet pcap."""
    fingerprints = collections.Counter()
    with open(path, 'rb') as source:
        header = source.read(24)
        magic = header[:4]
        if magic in (b'\xd4\xc3\xb2\xa1', b'\x4d\x3c\xb2\xa1'):
            endian = '<'
        elif magic in (b'\xa1\xb2\xc3\xd4', b'\xa1\xb2\x3c\x4d'):
            endian = '>'
        else:
            raise ValueError('classic Ethernet pcap required')
        if len(header) != 24 or struct.unpack(endian + 'I', header[20:24])[0] != 1:
            raise ValueError('Ethernet pcap required')
        while True:
            record = source.read(16)
            if not record:
                break
            if len(record) != 16:
                raise ValueError('truncated pcap record header')
            captured = struct.unpack(endian + 'IIII', record)[2]
            packet = source.read(captured)
            if len(packet) != captured:
                raise ValueError('truncated pcap record')
            if len(packet) < 34 or packet[12:14] != b'\x08\x00':
                continue
            ip = packet[14:]
            ihl = (ip[0] & 15) * 4
            total = int.from_bytes(ip[2:4], 'big')
            if (ip[0] >> 4 != 4 or ihl < 20 or ip[9] != 17
                    or int.from_bytes(ip[6:8], 'big') & 0x3fff
                    or total < ihl + 8 or len(ip) < total):
                continue
            length = int.from_bytes(ip[ihl+4:ihl+6], 'big')
            if length < 8 or ihl + length > total:
                continue
            payload = ip[ihl+8:ihl+length]
            fingerprints[hashlib.sha256(payload).hexdigest()] += 1
    return fingerprints


def summarize(client, server, capture=None):
    traces = {'client': client, 'server': server}
    if any(not rows for rows in traces.values()):
        raise ValueError('both peers need nonempty control traces')
    ports = {}

    def port_label(port):
        return ports.setdefault(port, 'port-' + str(len(ports) + 1))

    report = {'directions': {}, 'clock_offset_server_minus_client_us': {},
              'limitations': ['Successful send means kernel acceptance only.',
                              'Missing receive may include trace boundaries or incomplete logs.',
                              'Capture observation does not establish client wire arrival or an exact NAT timeout.']}
    bounds = {'lower': [], 'upper': []}
    for role, other in [('client', 'server'), ('server', 'client')]:
        incoming = collections.defaultdict(list)
        outcomes = collections.defaultdict(list)
        for row in traces[other]:
            if row['stage'] == 'receive':
                incoming[row['packet']].append(row)
            if row['stage'] in ('decode', 'state', 'ping'):
                outcomes[row['packet']].append(row)
        groups = collections.defaultdict(collections.Counter)
        missing = []
        failures = []
        for row in traces[role]:
            if row['stage'] != 'send':
                continue
            key = port_label(row['port'])
            counts = groups[key]
            counts['attempted'] += 1
            if not row['sent']:
                counts['send_failed'] += 1
                continue
            counts['sent'] += 1
            if capture is not None:
                counts['server_capture_seen' if row['packet'] in capture
                       else 'server_capture_missing'] += 1
            receives = incoming[row['packet']]
            control = row['control']
            detail = {k: v for k, v in control.items() if k != 'session'}
            detail['port'] = key
            detail['sender_mono_us'] = row['mono_us']
            if not receives:
                counts['missing_receive'] += 1
                if capture is not None:
                    detail['server_capture_seen'] = row['packet'] in capture
                missing.append(detail)
                continue
            counts['received'] += 1
            counts['duplicate_receives'] += len(receives) - 1
            for received in receives:
                delta = received['unix_us'] - row['unix_us']
                bounds['upper' if role == 'client' else 'lower'].append(
                    delta if role == 'client' else -delta)
            results = outcomes[row['packet']]
            for result in results:
                counts[result['stage'] + ':' + result['result']] += 1
                if result['stage'] == 'state' and result['result'] != 'accepted':
                    failures.append(dict(detail, result=result['result'],
                                         pending=result['pending']))
            if not results:
                counts['missing_decode_event'] += 1
        report['directions'][role + '_to_' + other] = {
            'ports': dict(groups), 'missing_receive': missing,
            'ignored_states': failures,
        }
    lower = max(bounds['lower'], default=None)
    upper = min(bounds['upper'], default=None)
    report['clock_offset_server_minus_client_us'] = {
        'lower': lower, 'upper': upper,
        'consistent': lower is not None and upper is not None and lower <= upper,
        'assumption': 'stable relative clock offset during the run; nonnegative transit time',
    }
    # Join fresh PING sequences to their solicited STATE, including retries.
    server_sends = collections.defaultdict(list)
    client_results = collections.defaultdict(list)
    server_pings = {}
    server_probes = {}
    sessions = {}
    for row in server:
        control = row.get('control', {})
        if row['stage'] == 'send' and control.get('kind') == 'state' and control.get('echo'):
            server_sends[(control['session'], control['echo'])].append(row)
        if row['stage'] == 'ping':
            server_pings[row['packet']] = row['result']
        if row['stage'] == 'probe':
            server_probes[row['packet']] = row['result']
    for row in client:
        if row['stage'] in ('receive', 'decode', 'state'):
            client_results[row['packet']].append(row)
    report['ping_lifecycles'] = []
    report['probe_lifecycles'] = []
    for row in client:
        control = row.get('control', {})
        if row['stage'] != 'send' or control.get('kind') not in ('ping', 'probe'):
            continue
        session = sessions.setdefault(control['session'], 'session-' + str(len(sessions) + 1))
        replies = []
        for reply in server_sends[(control['session'], control['seq'])]:
            results = client_results[reply['packet']]
            detail = {'sent': reply['sent'],
                      'client_received': any(r['stage'] == 'receive' for r in results),
                      'decode': [r['result'] for r in results if r['stage'] == 'decode'],
                      'state': [r['result'] for r in results if r['stage'] == 'state']}
            if capture is not None:
                detail['server_capture_seen'] = reply['packet'] in capture
            replies.append(detail)
        kind = control['kind']
        results = server_pings if kind == 'ping' else server_probes
        report[kind+'_lifecycles'].append({
            'session': session, 'seq': control['seq'], 'port': port_label(row['port']),
            'client_mono_us': row['mono_us'], 'sent': row['sent'],
            'server_'+kind+'_result': results.get(row['packet']), 'replies': replies,
        })
    report['client_timer_events'] = [
        {k: row[k] for k in ('mono_us', 'pending_before', 'pending_after', 'fallback')}
        for row in client if row['stage'] == 'timer'
    ]
    return report


if __name__ == '__main__':
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    capture = capture_fingerprints(sys.argv[3]) if len(sys.argv) == 4 else None
    print(json.dumps(summarize(load(sys.argv[1]), load(sys.argv[2]), capture), indent=2))
