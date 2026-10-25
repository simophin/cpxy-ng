#!/usr/bin/env python3
"""Run inside client-netns.py; private raw output, endpoint-free summary.
Z4: ten minutes paced UDP + ping, ten minutes with no application traffic,
then MTU ping and reverse UDP. Comparison: bounded TCP and UDP both ways.
Operator owns remote server, capture, firewall and cleanup.
"""
import argparse
import datetime
import json
import pathlib
import re
import signal
import subprocess
import sys
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('mode', choices=['endurance', 'idle', 'comparison'])
p.add_argument('target')
p.add_argument('port', type=int)
p.add_argument('out')
p.add_argument('--start-at', choices=['tcp-up-p1','tcp-up-p4','udp-up','tcp-down-p1','tcp-down-p4','udp-down'], default='tcp-up-p1')
p.add_argument('--continue-on-error', action='store_true')
a = p.parse_args()
out = pathlib.Path(a.out)
out.mkdir(mode=0o700, parents=True, exist_ok=True)
summary = dict(mode=a.mode, start_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(), cases=[])

def save():
    (out/'summary.json').write_text(json.dumps(summary, indent=2))

def ping(name, count=20):
    r = subprocess.run(['ping', '-n', '-c', str(count), '-i', '.2', '-W', '2',
                        '-s', '1172', '-M', 'do', a.target], capture_output=True,
                       text=True, timeout=count*.2+8)
    (out/(name+'.txt')).write_text(r.stdout+r.stderr)
    m = re.search(r'(\d+) packets transmitted, (\d+) received', r.stdout)
    row = dict(sent=int(m[1]) if m else 0, received=int(m[2]) if m else 0)
    summary[name] = row
    save()
    if row['received'] == 0:
        raise RuntimeError('no ping replies')

def transfer(name, seconds, udp=False, reverse=False, streams=1):
    cmd = ['iperf3', '-c', a.target, '-p', str(a.port), '-t', str(seconds),
           '-i', '.5', '--connect-timeout', '5000', '-J']
    if udp:
        cmd += ['-u', '-b', '5M', '-l', '1000', '--get-server-output']
    else:
        cmd += ['-P', str(streams)]
    if reverse:
        cmd += ['-R']
    t = time.time()
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=seconds+25)
    (out/(name+'.json')).write_text(r.stdout)
    (out/(name+'-stderr.txt')).write_text(r.stderr)
    raw = json.loads(r.stdout)
    if r.returncode or 'error' in raw:
        raise RuntimeError('iperf failed; inspect private raw output')
    end = raw['end']
    received = end.get('sum_received', end.get('sum', {}))
    # UDP receiver intervals are server output for upload, local for download.
    receiver = raw if reverse or not udp else raw.get('server_output_json', {})
    intervals = [v['sum'] for v in receiver.get('intervals', []) if 'sum' in v]
    interval_source = 'receiver_json'
    if udp and not reverse and not intervals:
        # Older/default iperf servers send text to --get-server-output.
        pattern = r'^\[\s*\d+\]\s+([\d.]+)-([\d.]+)\s+sec\s+.*?\s+([\d.]+)\s+ms\s+(\d+)/(\d+)\s+\(([\d.]+)%\)'
        for match in re.finditer(pattern, raw.get('server_output_text', ''), re.M):
            interval_start, interval_end = float(match[1]), float(match[2])
            if not 0 < interval_end-interval_start <= 2:
                continue  # Exclude the whole-test summary row.
            lost, packets = int(match[4]), int(match[5])
            intervals.append(dict(start=interval_start, end=interval_end,
                seconds=interval_end-interval_start,
                bytes=(packets-lost)*1000, packets=packets, lost_packets=lost,
                lost_percent=float(match[6])))
        interval_source = 'receiver_text_rounded_2dp'
    if udp and not intervals:
        raise RuntimeError('missing UDP receiver intervals')
    summary['cases'].append(dict(name=name, start_epoch=t, end_epoch=time.time(),
        receiver_mbps=received['bits_per_second']/1e6, receiver_bytes=received['bytes'],
        sender_tcp_congestion=end.get('sender_tcp_congestion'),
        receiver_tcp_congestion=end.get('receiver_tcp_congestion'),
        socket_buffers={k:raw['start'].get(k) for k in ('sock_bufsize','sndbuf_actual','rcvbuf_actual')},
        iperf_cpu_percent=end.get('cpu_utilization_percent'),
        lost_percent=received.get('lost_percent'), lost_packets=received.get('lost_packets'),
        packets=received.get('packets'), jitter_ms=received.get('jitter_ms'),
        receiver_interval_source=interval_source,
        receiver_intervals=[{k:v.get(k) for k in ('start','end','seconds','bytes','packets','lost_packets','lost_percent')} for v in intervals]))
    save()
    if received['bytes'] == 0:
        raise RuntimeError('no receiver bytes')

ping('startup_ping')
if a.mode in ('endurance', 'idle'):
    if a.mode == 'endurance':
        with (out/'loaded-ping.txt').open('w') as f:
            proc = subprocess.Popen(['ping','-n','-i','.2',a.target], stdout=f, stderr=subprocess.STDOUT)
            try:
                transfer('active-udp-up', 600, udp=True)
            finally:
                proc.send_signal(signal.SIGINT)
                proc.wait(timeout=5)
    summary['idle_start_epoch'] = time.time()
    save()
    time.sleep(600)
    summary['idle_end_epoch'] = time.time()
    save()
    ping('post_idle_ping')
    transfer('post-idle-udp-down', 10, udp=True, reverse=True)
else:
    cases = [('tcp-'+direction+'-p'+str(streams), 20, False, reverse, streams)
             for reverse, direction in ((False,'up'),(True,'down')) for streams in (1,4)]
    cases.insert(2, ('udp-up', 60, True, False, 1))
    cases.append(('udp-down', 60, True, True, 1))
    first = [v[0] for v in cases].index(a.start_at)
    for name, seconds, udp, reverse, streams in cases[first:]:
        try:
            transfer(name, seconds, udp=udp, reverse=reverse, streams=streams)
        except (RuntimeError, subprocess.TimeoutExpired, ValueError) as error:
            summary.setdefault('failed_cases', []).append(dict(name=name,
                error_type=type(error).__name__, reason='iperf failed; inspect private raw artifacts'))
            save()
            if not a.continue_on_error:
                raise
summary['end_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
summary['status'] = 'completed_with_failures' if summary.get('failed_cases') else 'complete'
save()
if summary.get('failed_cases'):
    sys.exit(1)
