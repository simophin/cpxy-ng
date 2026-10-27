#!/usr/bin/env python3
"""Summarize interleaved ref/A/B router windows from router-windows.sh output."""
import collections
import json
import pathlib
import re
import statistics
import sys

OUT = pathlib.Path(sys.argv[1])
HZ = 100


def run_info(path):
    info = {'bytes': 0, 'streams_ok': 0, 'streams': 0}
    for line in path.read_text().splitlines():
        if line.startswith('LOAD_START'):
            info['start'] = float(line.split()[1])
        elif line.startswith('LOAD_END'):
            info['end'] = float(line.split()[1])
        elif line.startswith('RESULT'):
            f = dict(kv.split('=') for kv in line.split()[1:])
            info['streams'] += 1
            if f['code'] == '200' and f['bytes'] == '80000000':
                info['streams_ok'] += 1
            info['bytes'] += int(f['bytes'])
        elif line.startswith('PING') and 'packets transmitted' in line:
            m = re.search(r'(\d+) packets transmitted, (\d+) packets received', line)
            info['ping_sent'], info['ping_recv'] = map(int, m.groups())
        elif line.startswith('PING') and 'min/avg/max' in line:
            info['ping_min'], info['ping_avg'], info['ping_max'] = map(float, line.split('=')[1].split()[0].split('/'))
        elif line.startswith(('STATUS_BEFORE', 'STATUS_AFTER')):
            key = line.split()[0].lower()
            info[key] = json.loads(line[line.index('{'):])
            info[key + '_time'] = line.split()[4]
    return info


def busy(ticks):
    return sum(ticks) - ticks[3] - ticks[4]


def profile_window(path, start, end):
    samples = [json.loads(l) for l in path.read_text().splitlines() if '"profile_sample"' in l]
    inside = [s for s in samples if start - 0.6 <= s['uptime_seconds'] <= end + 0.6]
    a, b = inside[0], inside[-1]
    wall = b['uptime_seconds'] - a['uptime_seconds']
    r = {'wall': wall}
    for cpu in ('cpu0', 'cpu1'):
        r[cpu] = (busy(b['cpu_ticks'][cpu]) - busy(a['cpu_ticks'][cpu])) / HZ / wall
    d = [y - x for x, y in zip(a['cpu_ticks']['cpu'], b['cpu_ticks']['cpu'])]
    r['sys_busy_cores'] = busy(d) / HZ / wall
    r['user_cores'], r['system_cores'] = (d[0] + d[1]) / HZ / wall, d[2] / HZ / wall
    r['irq_softirq_cores'] = (d[5] + d[6]) / HZ / wall
    ta = {t['tid']: t for t in a['threads']}
    groups = collections.Counter()
    for t in b['threads']:
        if t['tid'] in ta:
            ticks = t['user_ticks'] + t['system_ticks'] - ta[t['tid']]['user_ticks'] - ta[t['tid']]['system_ticks']
            groups['process'] += ticks
            name = t['name'] if t['name'] in ('rtp-worker', 'rtp-engine') else 'other'
            groups[name] += ticks
            if t['name'] == 'rtp-worker':
                r['worker_vcsw'] = (t.get('voluntary_ctxt_switches', 0) - ta[t['tid']].get('voluntary_ctxt_switches', 0)) / wall
    for k, v in groups.items():
        r[k + '_cores'] = v / HZ / wall
    na, nb = a['network_counters'], b['network_counters']
    r['udp6_rcvbuf_errors'] = nb.get('Udp6RcvbufErrors', 0) - na.get('Udp6RcvbufErrors', 0)
    ia, ib = a.get('interfaces', {}).get('rtpr0'), b.get('interfaces', {}).get('rtpr0')
    if ia and ib:
        r['tun_tx_drop'] = ib['tx_drop'] - ia['tx_drop']
    sa = {s['inode']: s['drops'] for s in a['sockets']}
    r['socket_drops'] = sum(s['drops'] - sa[s['inode']] for s in b['sockets'] if s['inode'] in sa)
    r['ctxt_per_s'] = (b['ctxt'] - a['ctxt']) / wall
    return r


def status_delta(info):
    before, after = info.get('status_before'), info.get('status_after')
    if not before or not after or not after.get('worker') or not before.get('worker'):
        return {}
    d = {}
    for k in ('sent_packets', 'received_packets'):
        d[k] = after[k] - before[k]
    for group in ('worker', 'shim_tun', 'shim_pair'):
        if group not in after or group not in before:
            continue
        for k, v in after[group].items():
            # High-water marks are maxima, not cumulative counts.
            d[f'{group}.{k}'] = v if k.endswith('_high') else v - before[group].get(k, 0)
    return d


rows = []
for run in sorted(OUT.glob('w*.run.txt')):
    tag = run.name[:-len('.run.txt')]
    window, binary, phase = tag.split('-')
    info = run_info(run)
    prof = profile_window(OUT / f'{tag}.profile.jsonl', info['start'], info['end'])
    duration = info['end'] - info['start']
    row = dict(window=int(window[1:]), binary=binary, phase=phase, duration=duration, **prof)
    row.update({k: info[k] for k in info if k.startswith(('ping', 'streams'))})
    if phase != 'idle':
        row['mbit'] = info['bytes'] * 8 / 1e6 / duration
        row['busy_core_ms_per_MB'] = row['sys_busy_cores'] * 1000 / (info['bytes'] / 1e6 / duration)
        row['process_core_ms_per_MB'] = row['process_cores'] * 1000 / (info['bytes'] / 1e6 / duration)
    row['status'] = status_delta(info)
    rows.append(row)

(OUT / 'summary.json').write_text(json.dumps(rows, indent=1))

def fmt(values):
    values = [v for v in values if v is not None]
    if not values:
        return '-'
    m = statistics.median(values)
    return f'{m:.3g} ({min(values):.3g}–{max(values):.3g})' if len(values) > 1 else f'{m:.3g}'

metrics = ['mbit', 'sys_busy_cores', 'process_cores', 'rtp-worker_cores', 'rtp-engine_cores',
           'busy_core_ms_per_MB', 'process_core_ms_per_MB', 'system_cores', 'irq_softirq_cores',
           'worker_vcsw', 'ctxt_per_s', 'udp6_rcvbuf_errors', 'socket_drops', 'tun_tx_drop',
           'ping_avg', 'ping_max', 'ping_recv', 'streams_ok']
for phase in ('idle', 'fixed', 'sat'):
    print(f'\n## {phase}')
    print('| metric | ' + ' | '.join(['ref', 'A', 'B']) + ' |')
    print('|---|---|---|---|')
    for m in metrics:
        cells = [fmt([r.get(m) for r in rows if r['phase'] == phase and r['binary'] == b]) for b in ('ref', 'A', 'B')]
        if any(c != '-' for c in cells):
            print(f'| {m} | ' + ' | '.join(cells) + ' |')

print('\n## worker/shim counters per window (A/B), per received+sent packet where noted')
for r in rows:
    s = r['status']
    if not s:
        continue
    pk = s['sent_packets'] + s['received_packets']
    keys = [k for k in s if '.' in k]
    print(f"w{r['window']}-{r['binary']}-{r['phase']} packets={pk} sent={s['sent_packets']} recv={s['received_packets']} "
          + ' '.join(f"{k.replace('worker.', 'w.')}={s.get(k)}" for k in keys))
