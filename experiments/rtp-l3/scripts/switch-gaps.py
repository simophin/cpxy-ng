#!/usr/bin/env python3
"""Endpoint-free 1000-byte inner UDP packet gaps across server port switches.
Usage: switch-gaps.py PRIVATE_ETHERNET_PCAP SERVER_IPV4 BASE_PORT
Excludes small control/ACK packets by exact expected v4 IPv4 length (1084).
Passive capture gaps are not final receiver loss. No switch is inferred across
a gap >= 5 seconds (separate benchmark phases); publish capture drop counts.
"""
import collections
import ipaddress
import json
import struct
import sys

server = ipaddress.IPv4Address(sys.argv[2]).packed
base = int(sys.argv[3])
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
            rows[direction].append((sec+fraction/scale, port))

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
        switches=switches, excluded_phase_or_outage_gaps=excluded)
print(json.dumps(summary, indent=2))
