#!/usr/bin/env python3
"""Bounded Linux packet-path samples, locally or over SSH (no remote Python).

Writes JSONL with counters, never process arguments, credentials or packet data.
Socket identity is a process FD and inode, not a peer address. CPU values are
cumulative USER_HZ ticks; compare deltas over monotonic_ns, accounting for all
cores. Socket drops are cumulative kernel counters and reset with the inode.
"""
import argparse
import json
import pathlib
import subprocess
import time


def collector(pid, count, interval):
    # Only numeric validated arguments are interpolated into this remote script.
    return f'''set -eu
pid={pid}
i=0
while [ "$i" -lt {count} ] && [ -d /proc/$pid/task ]; do
    echo BEGIN
    echo UPTIME; cat /proc/uptime
    echo CPU; cat /proc/stat
    echo SOFTIRQ; cat /proc/softirqs
    echo SNMP; cat /proc/$pid/net/snmp /proc/$pid/net/snmp6
    echo NETDEV; cat /proc/$pid/net/dev
    for task in /proc/$pid/task/*; do
        [ -r "$task/stat" ] || continue
        echo TASK; cat "$task/stat" 2>/dev/null || true
        echo STATUS; cat "$task/status" 2>/dev/null || true
        echo SCHEDSTAT; cat "$task/schedstat" 2>/dev/null || true
    done
    echo FDS
    for fd in /proc/$pid/fd/*; do
        link=$(readlink "$fd" 2>/dev/null) || continue
        case "$link" in socket:*) echo "${{fd##*/}} $link";; esac
    done
    echo UDP; cat /proc/$pid/net/udp
    echo UDP6; cat /proc/$pid/net/udp6
    echo END
    i=$((i + 1))
    [ "$i" -ge {count} ] || sleep {interval}
done
'''


def parse_sample(lines):
    result = {'cpu_ticks': {}, 'softirq': {}, 'threads': [], 'sockets': [],
              'network_counters': {}, 'interfaces': {}}
    sections = {'UPTIME', 'CPU', 'SOFTIRQ', 'SNMP', 'NETDEV', 'TASK', 'STATUS',
                'SCHEDSTAT', 'FDS', 'UDP', 'UDP6'}
    section = None
    fds = {}
    headers = {}
    for line in lines:
        if line in sections:
            section = line
            continue
        fields = line.split()
        if not fields:
            continue
        if section == 'UPTIME':
            result['uptime_seconds'] = float(fields[0])
        elif section == 'CPU':
            if fields[0].startswith('cpu'):
                result['cpu_ticks'][fields[0]] = list(map(int, fields[1:]))
            elif fields[0] in ('ctxt', 'intr', 'processes'):
                result[fields[0]] = int(fields[1])
        elif section == 'SOFTIRQ' and ':' in fields[0]:
            result['softirq'][fields[0].rstrip(':')] = list(map(int, fields[1:]))
        elif section == 'SNMP':
            if fields[0] in ('Udp:', 'Ip:', 'Tcp:', 'Icmp:'):
                name = fields[0].rstrip(':')
                if not fields[1].isdigit():
                    headers[name] = fields[1:]
                elif name in headers:
                    result['network_counters'][name] = dict(zip(headers[name], map(int, fields[1:])))
            elif fields[0].startswith(('Udp6', 'Ip6')):
                result['network_counters'][fields[0]] = int(fields[1])
        elif section == 'NETDEV' and ':' in line:
            # Interface packet and drop counters; a TUN's tx_drop is its full device queue.
            name, values = line.split(':', 1)
            values = list(map(int, values.split()))
            if len(values) >= 12:
                result['interfaces'][name.strip()] = {
                    'rx_packets': values[1], 'rx_drop': values[3],
                    'tx_packets': values[9], 'tx_drop': values[11]}
        elif section == 'TASK':
            # comm can contain spaces and parentheses; stat fields start after the last ')'.
            tail = line.rsplit(')', 1)[1].split()
            result['threads'].append({'tid': int(line.split(' ', 1)[0]),
                'name': line[line.index('(') + 1:line.rindex(')')],
                'user_ticks': int(tail[11]), 'system_ticks': int(tail[12]),
                'start_ticks': int(tail[19]), 'rss_pages': int(tail[21])})
        elif section == 'STATUS' and result['threads']:
            if fields[0] in ('voluntary_ctxt_switches:', 'nonvoluntary_ctxt_switches:', 'VmRSS:'):
                result['threads'][-1][fields[0].rstrip(':')] = int(fields[1])
        elif section == 'SCHEDSTAT' and result['threads'] and len(fields) == 3:
            result['threads'][-1]['schedstat'] = list(map(int, fields))
        elif section == 'FDS' and fields[1].startswith('socket:['):
            fds[fields[1][8:-1]] = int(fields[0])
        elif section in ('UDP', 'UDP6') and len(fields) >= 13 and fields[9] in fds:
            tx, rx = fields[4].split(':')
            result['sockets'].append({'fd': fds[fields[9]], 'inode': int(fields[9]),
                'family': section.lower(), 'tx_queue_bytes': int(tx, 16),
                'rx_queue_bytes': int(rx, 16), 'drops': int(fields[12])})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pid', type=int)
    parser.add_argument('--samples', type=int, default=11)
    parser.add_argument('--interval', type=float, default=1)
    parser.add_argument('--ssh', metavar='HOST')
    parser.add_argument('--ssh-config', type=pathlib.Path)
    args = parser.parse_args()
    if args.pid <= 0 or not 1 <= args.samples <= 3601 or not .1 <= args.interval <= 60:
        parser.error('require positive PID, 1..3601 samples, 0.1..60 second interval')
    command = ['sh']
    if args.ssh:
        command = ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10']
        if args.ssh_config:
            command += ['-F', str(args.ssh_config)]
        command += [args.ssh, 'sh']
    print(json.dumps({'event': 'profile_metadata', 'pid': args.pid,
        'interval_seconds': args.interval, 'requested_samples': args.samples,
        'scope': 'remote' if args.ssh else 'local',
        'cpu_units': 'USER_HZ ticks (Linux /proc: 100 per second)',
        'syscall_rates': 'unavailable: collect separately with perf/strace if supported',
        'socketpair_queues': 'unavailable in proc UDP tables; use worker queue metrics'}), flush=True)
    # Only the bounded script is supplied on stdin, not credentials or environment values.
    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as process:
        process.stdin.write(collector(args.pid, args.samples, args.interval))
        process.stdin.close()
        lines = []
        completed = 0
        for line in process.stdout:
            line = line.rstrip('\n')
            if line == 'BEGIN':
                lines = []
            elif line == 'END':
                sample = parse_sample(lines)
                sample.update(event='profile_sample', received_monotonic_ns=time.monotonic_ns())
                print(json.dumps(sample), flush=True)
                completed += 1
            else:
                lines.append(line)
        if process.wait():
            raise SystemExit(process.returncode)
        if completed != args.samples:
            raise SystemExit(f'process exited early: {completed}/{args.samples} samples')


if __name__ == '__main__':
    main()
