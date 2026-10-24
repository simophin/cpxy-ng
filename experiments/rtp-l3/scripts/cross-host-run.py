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
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('mode', choices=['endurance', 'comparison'])
p.add_argument('target')
p.add_argument('port', type=int)
p.add_argument('out')
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
    summary['cases'].append(dict(name=name, start_epoch=t, end_epoch=time.time(),
        receiver_mbps=received['bits_per_second']/1e6, receiver_bytes=received['bytes'],
        lost_percent=received.get('lost_percent'), lost_packets=received.get('lost_packets'),
        packets=received.get('packets'), jitter_ms=received.get('jitter_ms'),
        receiver_intervals=[{k:v.get(k) for k in ('start','end','seconds','bytes','packets','lost_packets','lost_percent')} for v in intervals]))
    save()
    if received['bytes'] == 0:
        raise RuntimeError('no receiver bytes')

ping('startup_ping')
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
    for reverse in (False, True):
        for streams in (1,4):
            transfer('tcp-%s-p%d'%('down' if reverse else 'up', streams), 20, reverse=reverse, streams=streams)
        transfer('udp-%s'%('down' if reverse else 'up'), 60, udp=True, reverse=reverse)
summary['end_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
save()
