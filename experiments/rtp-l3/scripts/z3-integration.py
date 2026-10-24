#!/usr/bin/env python3
"""Two clients behind disposable Linux masquerading router; no host changes.
Usage: timeout 360 unshare -Urn python3 z3-integration.py CRATE KEY OUTPUT
Requires ip, iptables, nsenter, unshare and iperf3; KEY must be outside CRATE.
OUTPUT is private raw evidence. summary.json contains endpoint-free aggregates.
"""
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess as sp
import sys
import time

crate, key, out = map(lambda s: Path(s).resolve(), sys.argv[1:])
out.mkdir(mode=0o700, parents=True, exist_ok=True)
os.chmod(out, 0o700)
assert not key.is_relative_to(crate), 'keep keys outside checkout'
binary = crate / 'target/release/rtp-l3-poc'
processes = []
files = []

def interrupted(signum, frame):
    raise SystemExit(128 + signum)

signal.signal(signal.SIGTERM, interrupted)
signal.signal(signal.SIGINT, interrupted)
env = dict(os.environ, POC_TRACE_PATHS='1', POC_KEEPALIVE_SECS='5',
           POC_TEST_DROP_ANNOUNCEMENTS_FILE=str(out/'drop-announcements'))


def run(args, ns=None, **kw):
    return sp.run(prefix(ns) + list(map(str, args)), check=True, timeout=15, **kw)


def prefix(ns):
    return ['nsenter', '-t', str(ns), '-n', '--'] if ns else []


def start(args, name, ns=None):
    log = open(out / name, 'w')
    files.append(log)
    p = sp.Popen(prefix(ns) + list(map(str, args)), stdout=log, stderr=log,
                 env=env, start_new_session=True)
    processes.append(p)
    return p


def stop(p):
    if p.poll() is None:
        os.killpg(p.pid, signal.SIGTERM)
        try:
            p.wait(timeout=4)
        except sp.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            p.wait(timeout=4)


def wait_for(fn, seconds=25):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = fn()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError('bounded wait expired')


def events(name, kind=None):
    path = out / name
    # Ignore a last line still being written by a live runtime.
    rows = [json.loads(s) for s in path.read_text().splitlines(keepends=True)
            if s.startswith('{') and s.endswith('\n')]
    return [e for e in rows if kind is None or e.get('event') == kind]


def namespace(name):
    p = start(['unshare', '-n', 'sleep', '350'], name + '.log')
    # Wait for unshare to replace its inherited namespace.
    wait_for(lambda: os.readlink(f'/proc/{p.pid}/ns/net') != os.readlink('/proc/self/ns/net'))
    run(['ip', 'link', 'set', 'lo', 'up'], p.pid)
    return p.pid


def link(name, a, b, subnet):
    run(['ip', 'link', 'add', name+'a', 'type', 'veth', 'peer', 'name', name+'b'])
    for suffix, ns, host in [('a', a, 1), ('b', b, 2)]:
        dev = name + suffix
        if ns:
            run(['ip', 'link', 'set', dev, 'netns', ns])
        run(['ip', 'addr', 'add', f'198.18.{subnet}.{host}/30', 'dev', dev], ns)
        run(['ip', 'link', 'set', dev, 'up'], ns)


def ipt(args):
    run(['iptables', '-w', *args], router)


def ping(i):
    # Large inner packets cannot match the short-control fault rule.
    run(['nsenter', '-t', app[i], '-U', '--preserve-credentials', '-n', '--', 'ping', '-c', '2', '-W', '2',
         '-s', '1000', '192.0.2.1'], stdout=sp.DEVNULL)


def paths(i):
    return events(f'client{i}.log', 'path')


def counters(i):
    return [e for e in events(f'client{i}.log') if e.get('event') in ('stats', 'final')][-1]


def server(name):
    p = start([binary, 'server', key, 'z3tun0', '192.0.2.1/24',
               '198.18.0.1:45983', '45990-46020', '10'], name)
    wait_for(lambda: events(name, 'ready'))
    return p


def control_rule(action, i):
    ipt([action, 'FORWARD', '-d', f'198.18.{i}.2', '-p', 'udp', '-m',
         'length', '--length', '104:260', '-j', 'DROP'])


def flush_conntrack():
    # NFNL_SUBSYS_CTNETLINK / IPCTNL_MSG_CT_DELETE, NLM_F_REQUEST|ACK.
    # No attributes means flush this network namespace's conntrack table.
    code = '''import socket,struct
s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,12)
s.bind((0,0));s.settimeout(3)
s.sendto(struct.pack("IHHII",20,258,5,1,0)+struct.pack("BBH",socket.AF_INET,0,0),(0,0))
r=s.recv(65536)
assert struct.unpack_from("H",r,4)[0]==2 and struct.unpack_from("i",r,16)[0]==0,repr(r)
'''
    run(['python3', '-c', code], router)


summary = {'protocol': 'xchacha20-v4', 'rotation_mean_seconds': 10,
           'keepalive_mean_seconds': 5, 'transfers': [], 'checks': {}}
try:
    run(['ip', 'link', 'set', 'lo', 'up'])
    router = namespace('router')
    clients = {i: namespace(f'outer-client{i}') for i in (1, 2)}
    link('wan', None, router, 0)
    for i in clients:
        link(f'lan{i}', router, clients[i], i)
        run(['ip', 'route', 'add', 'default', 'via', f'198.18.{i}.1'], clients[i])
    run(['sh', '-c', 'echo 1 > /proc/sys/net/ipv4/ip_forward'], router)
    ipt(['-t', 'nat', '-A', 'POSTROUTING', '-o', 'wanb', '-p', 'udp',
         '-j', 'MASQUERADE', '--to-ports', '51000-51999'])
    srv = server('server.log')
    for i in clients:
        start(['iperf3', '-s', '-B', '192.0.2.1', '-p', 45200+i, '-J', '-i', '0.5'], f'iperf{i}.log')
        script = out / f'app{i}.sh'
        script.write_text(f'echo $$ > "{out}/app{i}.pid"\nexec sleep 330\n')
        start(['python3', crate/'scripts/client-netns.py', binary, key,
               '198.18.0.1:45983', f'z3tun{i}', f'192.0.2.{i+1}/24', script,
               '45985'], f'client{i}.log', clients[i])
    app = {i: int(wait_for(lambda i=i: (out/f'app{i}.pid').read_text()
                          if (out/f'app{i}.pid').exists() else None)) for i in clients}
    for i in clients:
        wait_for(lambda i=i: paths(i))
        ping(i)
    # Two simultaneous clients use the same local source port behind MASQUERADE.
    for reverse in (False, True):
        jobs = []
        initial = {i: len(paths(i)) for i in clients}
        for i in clients:
            name = f'transfer-{i}-' + ('down' if reverse else 'up')
            args = ['nsenter', '-t', app[i], '-U', '--preserve-credentials', '-n', '--', 'iperf3', '-c',
                    '192.0.2.1', '-p', 45200+i, '-n', '104857600', '-b', '16M',
                    '-i', '0.5', '--get-server-output', '-J'] + (['-R'] if reverse else [])
            jobs.append((i, name, start(args, name+'.json')))
        for i, name, job in jobs:
            assert job.wait(timeout=90) == 0, name
            result = json.loads((out/(name+'.json')).read_text())
            assert 'error' not in result, result.get('error')
            received = result['end']['sum_received']
            assert received['bytes'] >= 104857600, received
            receiver = result if reverse else result['server_output_json']
            idle = maximum = 0
            for interval in receiver['intervals']:
                s = interval['sum']
                idle = idle + s['seconds'] if s['bytes'] == 0 else 0
                maximum = max(maximum, idle)
            # With 0.5s samples, positive intervals on either side add <1s uncertainty.
            assert maximum + 1 < 5, (name, maximum)
            ports = {p['port'] for p in paths(i)[initial[i]:]}
            assert len(ports) >= 3, (name, ports)
            summary['transfers'].append({'client': i, 'direction': 'down' if reverse else 'up',
                'received_bytes': received['bytes'], 'seconds': received['seconds'],
                'distinct_new_rotation_ports': len(ports), 'zero_byte_interval_run_seconds': maximum,
                'stall_upper_bound_seconds': maximum + 1})
    summary['checks']['concurrent_masquerade_rotation_transfers'] = True

    # Lose initial announcement and +1/+2 retries; a retry or PING recovers.
    n = len(events('server.log', 'rotation'))
    control_rule('-A', 1)
    wait_for(lambda: len(events('server.log', 'rotation')) > n, 17)
    before = len(paths(1))
    time.sleep(2.5)
    control_rule('-D', 1)
    recovered = wait_for(lambda: paths(1)[before:] or None, 4)
    summary['announcement_recovery'] = 'ping' if recovered[0]['solicited'] else 'retry'
    ping(1)
    summary['checks']['dropped_announcement_retry_or_ping'] = True

    # Selectively lose ALL unsolicited STATEs (including subsequent rotations),
    # preserving PING replies. The explicit test hook avoids ciphertext/size
    # classification and proves that PING alone supplies the missed secret.
    n = len(events('server.log', 'rotation'))
    marker = out/'drop-announcements'
    marker.touch(mode=0o600)
    before = {i: len(paths(i)) for i in clients}
    wait_for(lambda: len(events('server.log', 'rotation')) > n, 17)
    for i in clients:
        recovered = wait_for(lambda i=i: paths(i)[before[i]:] or None, 15)
        assert all(p['solicited'] for p in recovered), recovered
        ping(i)
    drops = len(events('server.log', 'test_announcement_drop'))
    assert drops >= 2, drops
    marker.unlink()
    summary['test_hook_announcement_drops'] = drops
    summary['checks']['missed_secret_recovered_by_ping_both_clients'] = True

    # Force disjoint translated source ports and actually flush conntrack.
    ipt(['-t', 'nat', '-R', 'POSTROUTING', '1', '-o', 'wanb', '-p', 'udp',
         '-j', 'MASQUERADE', '--to-ports', '52000-52999'])
    before = len(events('server.log', 'registration'))
    flush_conntrack()
    wait_for(lambda: len({e['session'] for e in events('server.log', 'registration')[before:]
                         if e['tuple_changed'] and e['same_port']}) == 2, 15)
    for i in clients:
        ping(i)
    summary['checks']['conntrack_flush_same_port_authenticated_rebind_both_clients'] = True

    # Block the complete rotation range to prevent a subsequent rotation from
    # bypassing the fault; the base remains reachable. Verify fallback counters.
    before = {i: counters(i)['fallbacks'] for i in clients}
    registration_start = len(events('server.log', 'registration'))
    ipt(['-A', 'FORWARD', '-p', 'udp', '--dport', '45990:46020', '-j', 'DROP'])
    ipt(['-A', 'FORWARD', '-p', 'udp', '--sport', '45990:46020', '-j', 'DROP'])
    wait_for(lambda: all(counters(i)['fallbacks'] > before[i] for i in clients)
             and len({e['session'] for e in events('server.log', 'registration')[registration_start:]
                      if e['port'] == 45983}) == 2, 25)
    ipt(['-D', 'FORWARD', '-p', 'udp', '--dport', '45990:46020', '-j', 'DROP'])
    ipt(['-D', 'FORWARD', '-p', 'udp', '--sport', '45990:46020', '-j', 'DROP'])
    for i in clients:
        wait_for(lambda i=i: (ping(i) or True))
    summary['checks']['blocked_rotation_base_fallback_both_clients'] = True

    before = {i: len(paths(i)) for i in clients}
    stop(srv)
    srv = server('server-restart.log')
    for i in clients:
        wait_for(lambda i=i: any(p['instance_changed'] for p in paths(i)[before[i]:]), 25)
        ping(i)
    summary['checks']['live_clients_server_instance_restart'] = True
    for p in reversed(processes):
        stop(p)
    summary['client_final_counters'] = [counters(i) for i in clients]
    summary['server_final_counters'] = [events(n, 'final')[-1] for n in ('server.log', 'server-restart.log')]
    assert all(e['session_conflicts'] == 0 for e in summary['server_final_counters'])
    (out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print('PASS Z3 namespace integration')
finally:
    for p in reversed(processes):
        stop(p)
    for f in files:
        f.close()
