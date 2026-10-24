#!/usr/bin/env python3
"""Endpoint-free 1000-byte inner UDP packet gaps across server port switches.
Usage: switch-gaps.py PRIVATE_ETHERNET_PCAP SERVER_IPV4 BASE_PORT [CASE_SUMMARY_JSON [CLIENT_TRACE_JSONL]]
Excludes small control/ACK packets by exact expected IPv4 length (1084).
Passive capture gaps are not final receiver loss. Port transitions can include reordered packets, and are not runtime switch counts.
No transition is inferred across
a gap >= 5 seconds (separate benchmark phases); publish capture drop counts.
"""
import collections
import datetime
import ipaddress
import json
import struct
import sys

server = ipaddress.IPv4Address(sys.argv[2]).packed
base = int(sys.argv[3])
windows = []
cases = []
case_summary = {}
if len(sys.argv) > 4:
    case_summary = json.loads(open(sys.argv[4]).read())
    cases = case_summary['cases']
    offset = case_summary.get('capture_minus_client_clock_seconds', 0)
    windows = [(v['start_epoch']+offset, v['end_epoch']+offset)
               for v in cases if 'udp' in v['name']]
    if not windows:
        sys.exit('case summary has no UDP windows')
rows = collections.defaultdict(list)
base_counts = collections.Counter()
with open(sys.argv[1], 'rb') as f:
    h = f.read(24)
    endian = '<' if h[:4] in (b'\xd4\xc3\xb2\xa1', b'\x4d\x3c\xb2\xa1') else '>'
    scale = 1e9 if h[:4] in (b'\x4d\x3c\xb2\xa1', b'\xa1\xb2\x3c\x4d') else 1e6
    if len(h) != 24 or struct.unpack(endian+'I', h[20:24])[0] != 1:
        sys.exit('Ethernet pcap required')
    while True:
        h = f.read(16)
        if not h:
            break
        sec, fraction, captured, _ = struct.unpack(endian+'IIII', h)
        packet = f.read(captured)
        if len(packet) != captured:
            sys.exit('truncated record')
        if len(packet) < 54 or packet[12:14] != b'\x08\x00':
            continue
        timestamp = sec+fraction/scale
        if windows and not any(start <= timestamp <= end for start, end in windows):
            continue
        ip = packet[14:]
        ihl = (ip[0]&15)*4
        if ip[0] >> 4 != 4 or ip[9] != 17 or len(ip) < ihl+20:
            continue
        if server not in (ip[12:16], ip[16:20]):
            continue
        src, dst = struct.unpack('!HH', ip[ihl:ihl+4])
        direction = 'download' if ip[12:16] == server else 'upload'
        port = src if direction == 'download' else dst
        if port == base:
            base_counts[direction] += 1
        if struct.unpack('!H', ip[2:4])[0] == 1084 and ip[ihl+8:ihl+10] == b'\x80\x60':
            rows[direction].append((timestamp, port))

summary = dict(base_port_udp_packets=dict(base_counts), directions={})
for direction, packets in rows.items():
    gaps = []
    switches = []
    excluded = []
    for (t, port), (n, next_port) in zip(packets, packets[1:]):
        gap = (n-t)*1000
        if gap >= 5000:
            excluded.append(dict(start_epoch=t, end_epoch=n, milliseconds=gap))
            continue
        gaps.append(gap)
        if port != next_port:
            switches.append(dict(epoch=n, milliseconds=gap))
    gaps.sort()
    summary['directions'][direction] = dict(packets=len(packets),
        ports_used=len({p for _,p in packets}),
        gap_ms={k:gaps[round((len(gaps)-1)*q)] for k,q in [('median',.5),('p99',.99),('max',1)]} if gaps else None,
        port_transitions=switches, excluded_phase_or_outage_gaps=excluded)
if len(sys.argv) > 5:
    origin = datetime.datetime.fromisoformat(case_summary['start_utc']).timestamp()
    offset = case_summary.get('capture_minus_client_clock_seconds', 0)
    nearby = []
    for line in open(sys.argv[5]):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if event.get('event') != 'path':
            continue
        client_epoch = origin + event['seconds']
        for case in cases:
            if 'udp' not in case['name'] or not case['start_epoch'] <= client_epoch <= case['end_epoch']:
                continue
            direction = 'download' if 'down' in case['name'] else 'upload'
            center = client_epoch + offset
            packets = [(t,p) for t,p in rows[direction] if abs(t-center) <= 3]
            gaps = [(b[0]-a[0])*1000 for a,b in zip(packets,packets[1:])]
            rel = client_epoch-case['start_epoch']
            intervals = [v for v in case['receiver_intervals']
                         if v['end'] >= rel-3 and v['start'] <= rel+3]
            lost = sum(v.get('lost_packets') or 0 for v in intervals)
            total = sum(v.get('packets') or 0 for v in intervals)
            nearby.append(dict(case=case['name'], runtime_seconds=event['seconds'],
                approximate_capture_epoch=center, direction=direction,
                window_half_width_seconds=3, capture_data_packets=len(packets),
                capture_max_gap_ms=max(gaps) if gaps else None,
                receiver_interval_lost_packets=lost, receiver_interval_packets=total,
                receiver_interval_loss_percent=100*lost/total if total else None,
                receiver_interval_count=len(intervals)))
    summary['runtime_switch_windows'] = nearby
    summary['runtime_switch_window_note'] = ('Approximate alignment: script start follows runtime startup; '
        'runtime timers have whole-second precision. +/-3 s windows include adjacent receiver intervals; '
        'they do not attribute all loss to switching. Caller supplies capture_minus_client_clock_seconds '
        'when clocks differ. Raw port transitions include reordering and are not rotation counts.')
print(json.dumps(summary, indent=2))
