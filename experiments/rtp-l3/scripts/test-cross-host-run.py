#!/usr/bin/env python3
"""Exercise report parsing and bounded continuation with fake CLI processes."""
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name('cross-host-run.py')


class Reporting(unittest.TestCase):
    def run_fixture(self, server_format):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            bin_dir = root/'bin'
            bin_dir.mkdir()
            interval = dict(start=0, end=1, seconds=1, bytes=9000, packets=10,
                            lost_packets=1, lost_percent=10)
            receiver = dict(bytes=9000, bits_per_second=72000, lost_percent=10,
                            lost_packets=1, packets=10, jitter_ms=.1)
            raw = dict(start={}, intervals=[dict(sum=interval)], end=dict(
                sum_received=receiver, sender_tcp_congestion='fixture-cubic',
                receiver_tcp_congestion='fixture-bbr', cpu_utilization_percent={'host_total': 1}))
            if server_format == 'json':
                raw['server_output_json'] = dict(intervals=[dict(sum=interval)])
            else:
                raw['server_output_text'] = ('[  5]   0.00-1.00 sec 8.79 KBytes 72.0 Kbits/sec 0.100 ms 1/10 (10%)\n'
                                             if server_format == 'text' else 'no receiver intervals')
            for name, source in {
                'ping': "print('20 packets transmitted, 20 received, 0% packet loss')\n",
                'iperf3': 'import json\nprint(json.dumps('+repr(raw)+'))\n',
            }.items():
                path = bin_dir/name
                path.write_text('#!'+sys.executable+'\n'+source)
                path.chmod(0o700)
            out = root/'out'
            outcome = subprocess.run([sys.executable, str(SCRIPT), 'comparison',
                                      '192.0.2.1', '12345', str(out), '--continue-on-error'],
                                     env=dict(os.environ, PATH=str(bin_dir)+os.pathsep+os.environ['PATH']),
                                     capture_output=True, text=True, timeout=10)
            return outcome, json.loads((out/'summary.json').read_text())

    def test_text_intervals_preserve_final_summary_metadata(self):
        outcome, summary = self.run_fixture('text')
        self.assertEqual(outcome.returncode, 0, outcome.stderr)
        self.assertEqual(summary['status'], 'complete')
        udp = next(v for v in summary['cases'] if v['name'] == 'udp-up')
        self.assertEqual(udp['receiver_interval_source'], 'receiver_text_rounded_2dp')
        self.assertEqual(udp['receiver_intervals'][0]['bytes'], 9000)
        self.assertEqual(udp['receiver_intervals'][0]['seconds'], 1)
        self.assertEqual(udp['sender_tcp_congestion'], 'fixture-cubic')
        self.assertEqual(udp['iperf_cpu_percent'], {'host_total': 1})

    def test_json_receiver_intervals(self):
        outcome, summary = self.run_fixture('json')
        self.assertEqual(outcome.returncode, 0, outcome.stderr)
        udp = next(v for v in summary['cases'] if v['name'] == 'udp-up')
        self.assertEqual(udp['receiver_interval_source'], 'receiver_json')
        self.assertEqual(udp['receiver_intervals'][0]['lost_packets'], 1)

    def test_missing_intervals_preserves_failure_and_continues(self):
        outcome, summary = self.run_fixture('missing')
        self.assertNotEqual(outcome.returncode, 0)
        self.assertEqual(summary['status'], 'completed_with_failures')
        self.assertEqual([v['name'] for v in summary['failed_cases']], ['udp-up'])
        self.assertEqual(len(summary['cases']), 5)


if __name__ == '__main__':
    unittest.main()
