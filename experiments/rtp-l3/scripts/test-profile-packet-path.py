#!/usr/bin/env python3
"""Validate proc field offsets and FD/inode attribution without a live router."""
import importlib.util
import pathlib
import sys
import unittest

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location('profile', pathlib.Path(__file__).with_name('profile-packet-path.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


class ParserTests(unittest.TestCase):
    def test_stat_offsets_and_socket_attribution(self):
        stat = ['S'] + [str(n) for n in range(4, 53)]
        result = profile.parse_sample([
            'UPTIME', '100.25 50.00', 'CPU', 'cpu0 1 2 3 4 5 6 7 8 9 10', 'ctxt 42',
            'SOFTIRQ', 'NET_RX: 12 34', 'TASK', '123 (worker (rx)) ' + ' '.join(stat),
            'STATUS', 'voluntary_ctxt_switches: 77', 'VmRSS: 4096 kB',
            'SCHEDSTAT', '100 200 300', 'FDS', '7 socket:[999]',
            'UDP', '0: 0100007F:1000 00000000:0000 07 00000010:00000020 00:00000000 00000000 0 0 999 2 0 3',
            'UDP6', '1: ADDR:1000 ADDR:0000 07 0:0 00:00000000 00000000 0 0 888 2 0 100',
            'SNMP', 'Udp: InDatagrams RcvbufErrors', 'Udp: 10 20', 'Udp6RcvbufErrors 30'])
        thread = result['threads'][0]
        self.assertEqual((thread['user_ticks'], thread['system_ticks'], thread['start_ticks'], thread['rss_pages']), (14, 15, 22, 24))
        self.assertEqual(thread['name'], 'worker (rx)')
        self.assertEqual(thread['voluntary_ctxt_switches'], 77)
        self.assertEqual(result['sockets'], [dict(fd=7, inode=999, family='udp', tx_queue_bytes=16, rx_queue_bytes=32, drops=3)])
        self.assertEqual(result['network_counters']['Udp']['RcvbufErrors'], 20)
        self.assertEqual(result['network_counters']['Udp6RcvbufErrors'], 30)
        self.assertNotIn('ADDR', str(result))

    def test_disappearing_task_and_socket(self):
        result = profile.parse_sample(['TASK', 'STATUS', 'Name: gone', 'FDS', 'UDP'])
        self.assertEqual(result['threads'], [])
        self.assertEqual(result['sockets'], [])


if __name__ == '__main__':
    unittest.main()
