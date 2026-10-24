#!/usr/bin/env python3
"""Summarize bounded Ethernet/IPv4 pcap without publishing endpoint identity.
Usage: capture-shape.py PRIVATE_PCAP SERVER_IPV4 [MAX_IPV4_LEN] > SANITIZED_SUMMARY.json
MAX_IPV4_LEN defaults to 1256 (1200-byte inner MTU + 56).
Capture observations are not authenticated receiver loss measurements.
"""
import collections
import datetime
import ipaddress
import json
import struct
import sys

server = ipaddress.IPv4Address(sys.argv[2]).packed
max_len = int(sys.argv[3]) if len(sys.argv) > 3 else 1256
flows = {}
ports = {}
non_rtp = collections.Counter()
first = last = None
with open(sys.argv[1], 'rb') as f:
    header = f.read(24)
    endian = '<' if header[:4] in (b'\xd4\xc3\xb2\xa1',b'\x4d\x3c\xb2\xa1') else '>'
    scale = 1e9 if header[:4] in (b'\x4d\x3c\xb2\xa1',b'\xa1\xb2\x3c\x4d') else 1e6
    if len(header)!=24 or struct.unpack(endian+'I',header[20:24])[0]!=1:
        sys.exit('Ethernet pcap required')
    while True:
        h=f.read(16)
        if not h:
            break
        sec, fraction, captured, original = struct.unpack(endian+'IIII',h)
        packet=f.read(captured)
        if len(packet)!=captured:
            sys.exit('truncated pcap record')
        t=sec+fraction/scale
        if len(packet)<42 or packet[12:14]!=b'\x08\x00':
            continue
        ip=packet[14:]
        ihl=(ip[0]&15)*4
        if ip[0]>>4!=4 or ip[9]!=17 or len(ip)<ihl+8:
            continue
        if server not in (ip[12:16], ip[16:20]):
            continue
        length=struct.unpack('!H',ip[2:4])[0]
        payload=ip[ihl+8:]
        direction='download' if ip[12:16]==server else 'upload'
        source_port, destination_port = struct.unpack('!HH', ip[ihl:ihl+4])
        port = source_port if direction == 'download' else destination_port
        first=t if first is None else min(first,t)
        last=t if last is None else max(last,t)
        if len(payload)<12 or payload[:2]!=b'\x80\x60':
            non_rtp[direction]+=1
            continue
        seq,stamp,ssrc=struct.unpack('!HII',payload[2:12])
        ports.setdefault((direction, port), []).append(t)
        k=(direction,port,ssrc)
        if k not in flows:
            flows[k]={'times':[],'lengths':[],'stamps':[],'indices':set(),'highest':None,'reorders':0,'duplicates':0}
        v=flows[k]
        h=v['highest']
        index=seq if h is None else (h&~65535)|seq
        if h is not None:
            if index+32768<h:index+=65536
            elif index>h+32768:index-=65536
            if index in v['indices']:v['duplicates']+=1
            elif index<h:v['reorders']+=1
        v['highest']=index if h is None else max(h,index)
        v['indices'].add(index)
        v['times'].append(t);v['lengths'].append(length);v['stamps'].append(stamp)

def quantiles(values):
    values=sorted(values)
    if not values:return None
    return {name:values[round((len(values)-1)*q)] for name,q in [('min',0),('median',.5),('p95',.95),('p99',.99),('max',1)]}

rows=[]
port_numbers = {p: i+1 for i, p in enumerate(sorted({p for _, p in ports}))}
for (direction,port,_),v in flows.items():
    gaps=[(b-a)*1000 for a,b in zip(v['times'],v['times'][1:])]
    bursts=[];n=1
    for gap in gaps:
        if gap<=1:n+=1
        else:bursts.append(n);n=1
    bursts.append(n)
    stamp_delta=[(b-a)&0xffffffff for a,b in zip(v['stamps'],v['stamps'][1:])]
    rows.append({'direction':direction,'server_port_number':port_numbers[port],
                 'session_number':len(rows)+1,'packets':len(v['times']),
                 'start_epoch':v['times'][0],'end_epoch':v['times'][-1],
                 'ipv4_length_bytes':quantiles(v['lengths']),
                 'ipv4_length_top_counts':collections.Counter(v['lengths']).most_common(8),
                 'ip_bytes':sum(v['lengths']),'max_expected_ipv4_len':max_len,'oversize_ip_packets':sum(n>max_len for n in v['lengths']),
                 'interarrival_ms':quantiles(gaps),'burst_packets_gap_at_most_1ms':quantiles(bursts),
                 'rtp_timestamp_delta_ticks':quantiles(stamp_delta),
                 'rtp_sequence_duplicates_observed':v['duplicates'],'rtp_reorder_observations':v['reorders'],
                 'rtp_sequence_missing_in_capture_span':max(v['indices'])-min(v['indices'])+1-len(v['indices']),
                 'note':'Passive capture gaps/reorder are not authenticated receiver/path loss; offload/capture boundaries may affect them.'})
port_rows = [{'direction': d, 'server_port_number': port_numbers[p], 'packets': len(times),
              'interarrival_ms': quantiles([(b-a)*1000 for a,b in zip(times,times[1:])])}
             for (d,p),times in sorted(ports.items())]
print(json.dumps({'start_utc':datetime.datetime.fromtimestamp(first,datetime.timezone.utc).isoformat() if first else None,
                  'end_utc':datetime.datetime.fromtimestamp(last,datetime.timezone.utc).isoformat() if last else None,
                  'non_rtp_udp_packets':dict(non_rtp),'server_ports':port_rows,'flows':rows},indent=2))
