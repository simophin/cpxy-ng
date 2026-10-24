#!/usr/bin/env python3
"""Bounded sequential iperf3/ping matrix; output directory must stay private.
No endpoints or raw outputs belong in Git. Summaries omit endpoint identity.
"""
import argparse
import datetime
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--target', required=True)
p.add_argument('--port', type=int, required=True)
p.add_argument('--out', required=True)
p.add_argument('--kind', choices=['direct', 'tunnel'], required=True)
p.add_argument('--protocol', choices=['all','tcp','udp'], default='all')
p.add_argument('--udp-direction', choices=['both','up','down'], default='both')
p.add_argument('--tcp-seconds', type=int, default=20)
p.add_argument('--repetitions', type=int, default=3)
p.add_argument('--udp-seconds', type=int, default=10)
p.add_argument('--udp-rates', default='5M,10M,20M,50M,100M')
a = p.parse_args()
if not (1 <= a.tcp_seconds <= 30 and 1 <= a.udp_seconds <= 20 and 1 <= a.repetitions <= 3):
    p.error('bounded duration/repetition limits exceeded')
out = pathlib.Path(a.out)
out.mkdir(parents=True, exist_ok=True)
out.chmod(0o700)

def cpu():
    pid = os.environ.get('POC_TUN_PID')
    if not pid:
        return None
    fields = pathlib.Path('/proc', pid, 'stat').read_text().rsplit(')', 1)[1].split()
    rss = pathlib.Path('/proc', pid, 'status').read_text()
    return {'cpu_seconds': (int(fields[11])+int(fields[12])) / os.sysconf('SC_CLK_TCK'),
            'rss_bytes': int(re.search(r'VmRSS:\s+(\d+)', rss)[1])*1024}

def ping_summary(text):
    values = [float(v) for v in re.findall(r'time[=<]([0-9.]+) ms', text)]
    values.sort()
    def q(f):
        return values[min(len(values)-1, round((len(values)-1)*f))] if values else None
    m = re.search(r'(\d+) packets transmitted, (\d+) received', text)
    return {'samples': len(values), 'median_ms': q(.5), 'p95_ms': q(.95), 'p99_ms': q(.99),
            'sent': int(m[1]) if m else None, 'received': int(m[2]) if m else None}

meta = {'kind': a.kind, 'protocol':a.protocol, 'udp_direction':a.udp_direction, 'start_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'clock_ticks': os.sysconf('SC_CLK_TCK'), 'tcp_seconds': a.tcp_seconds,
        'repetitions': a.repetitions, 'udp_seconds': a.udp_seconds,
        'udp_payload_bytes': 1000, 'udp_stop_loss_percent': 2,
        'udp_rates': a.udp_rates.split(',')}
for name in ('tcp_congestion_control','tcp_rmem','tcp_wmem','rmem_max','wmem_max'):
    file = pathlib.Path('/proc/sys/net', 'core' if name.endswith('_max') else 'ipv4', name)
    meta[name] = file.read_text().strip() if file.exists() else None
(out/'metadata.json').write_text(json.dumps(meta, indent=2))
idle = subprocess.run(['ping','-n','-c','100','-i','0.1','-w','15',a.target],
                      capture_output=True, text=True, timeout=20)
(out/'idle-ping.txt').write_text(idle.stdout+idle.stderr)
summary = {'kind': a.kind, 'idle_rtt': ping_summary(idle.stdout), 'cases': []}
(out/'summary.json').write_text(json.dumps(summary, indent=2))

def run_once(name, extra, duration, attributes):
    before = cpu()
    start = time.time()
    ping = subprocess.Popen(['ping','-n','-i','0.1','-w',str(duration+8),a.target],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        result = subprocess.run(['iperf3','-c',a.target,'-p',str(a.port),'-t',str(duration),
                                 '--connect-timeout','5000','-J']+extra,
                                capture_output=True, text=True, timeout=duration+25)
    finally:
        ping.send_signal(signal.SIGINT)
        ping_text = ping.communicate(timeout=5)[0]
    end = time.time()
    after = cpu()
    (out/(name+'.json')).write_text(result.stdout)
    (out/(name+'-stderr.txt')).write_text(result.stderr)
    (out/(name+'-ping.txt')).write_text(ping_text)
    raw = json.loads(result.stdout)
    if result.returncode or 'error' in raw:
        raise RuntimeError(name+': '+raw.get('error',result.stderr))
    received = raw['end'].get('sum_received',raw['end'].get('sum',{}))
    sent = raw['end'].get('sum_sent',raw['end'].get('sum',{}))
    if received.get('bytes',0) <= 0:
        raise RuntimeError(name+': no receiver bytes')
    row = dict(name=name, start_epoch=start, end_epoch=end, **attributes)
    row.update(receiver_mbps=received['bits_per_second']/1e6,
               receiver_bytes=received['bytes'], receiver_seconds=received['seconds'],
               retransmits=sent.get('retransmits'), loss_percent=received.get('lost_percent'),
               lost_packets=received.get('lost_packets'), packets=received.get('packets'),
               jitter_ms=received.get('jitter_ms'), out_of_order=received.get('out_of_order'),
               loaded_rtt=ping_summary(ping_text),
               iperf_cpu_percent=raw['end'].get('cpu_utilization_percent'))
    if before and after:
        row['client_tunnel_cpu_percent']=(after['cpu_seconds']-before['cpu_seconds'])/(end-start)*100
        row['client_tunnel_rss_bytes']=after['rss_bytes']
    summary['cases'].append(row)
    (out/'summary.json').write_text(json.dumps(summary,indent=2))
    print(json.dumps({'case':name,'receiver_mbps':row['receiver_mbps'],'loss_percent':row['loss_percent']}),flush=True)
    time.sleep(1)
    return row

def run(name, extra, duration, attributes):
    for attempt in (1,2):
        try:
            return run_once(name,extra,duration,attributes)
        except (RuntimeError,subprocess.TimeoutExpired) as error:
            summary.setdefault('failed_attempts',[]).append({'case':name,'attempt':attempt,'error':str(error),'epoch':time.time()})
            raw_path=out/(name+'.json')
            if raw_path.exists():
                raw_path.rename(out/(name+'-failed-attempt%d.json'%attempt))
            (out/'summary.json').write_text(json.dumps(summary,indent=2))
            if attempt==2:
                raise
            time.sleep(2)

if a.protocol != 'udp':
    for direction in ('up','down'):
        for streams in (1,4):
            for repetition in range(1,a.repetitions+1):
                run('tcp-%s-p%d-r%d'%(direction,streams,repetition),
                    ['-P',str(streams)]+(['-R'] if direction=='down' else []),a.tcp_seconds,
                    dict(protocol='tcp',direction=direction,streams=streams,repetition=repetition))
if a.protocol != 'tcp':
    for direction in (('up','down') if a.udp_direction=='both' else (a.udp_direction,)):
        for rate in a.udp_rates.split(','):
            row = run('udp-%s-%s'%(direction,rate), ['-u','-b',rate,'-l','1000']+
                      (['-R'] if direction=='down' else []),a.udp_seconds,
                      dict(protocol='udp',direction=direction,offered_rate=rate))
            if (row['loss_percent'] or 0) > 2:
                print('stopping UDP ladder after loss threshold',direction,flush=True)
                break
summary['end_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
(out/'summary.json').write_text(json.dumps(summary,indent=2))
