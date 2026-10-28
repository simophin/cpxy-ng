#!/usr/bin/env python3
"""Total inbound loss per window: kernel UDP drops plus worker device-queue drops."""
import json, sys, statistics
rows = json.load(open(sys.argv[1]))
out = {}
for r in rows:
    if r['phase'] == 'idle' or not r['status']:
        continue
    s = r['status']
    q = s.get('worker.write_queue_full', s.get('worker.device_queue_full', 0))
    udp = r['udp6_rcvbuf_errors']
    arrivals = s['received_packets'] + udp + q
    out.setdefault((r['binary'], r['phase']), []).append((udp, q, 100 * (udp + q) / arrivals))
for k, v in sorted(out.items()):
    print(k, 'udp', [x[0] for x in v], 'queue', [x[1] for x in v],
          'total pct median', round(statistics.median(x[2] for x in v), 1))
