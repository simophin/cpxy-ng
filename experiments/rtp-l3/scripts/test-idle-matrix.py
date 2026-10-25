#!/usr/bin/env python3
"""Real delayed UDP isolation plus evidence/capture and bounds fixtures."""
import importlib.util
import io
import json
import pathlib
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = pathlib.Path(__file__).parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / (name+'.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


matrix, evidence = load('idle-matrix'), load('idle-evidence')


def pcap(path, payloads, link=1):
    with open(path, 'wb') as stream:
        stream.write(struct.pack('<IHHIIII', 0xa1b2c3d4, 2, 4, 0, 0, 65535, link))
        for data in payloads:
            ip = bytearray(20)
            ip[0], ip[9] = 0x45, 17
            ip[2:4] = struct.pack('!H', 28+len(data))
            udp = struct.pack('!HHHH', 1234, 2345, len(data)+8, 0)+data
            headers = {1: b'\0'*12+b'\x08\x00', 101: b'',
                       113: b'\0'*14+b'\x08\x00', 276: b'\x08\x00'+b'\0'*18}
            packet = headers[link]+ip+udp
            stream.write(struct.pack('<IIII', 1, 0, len(packet), len(packet))+packet)


class IdleMatrixTest(unittest.TestCase):
    def test_real_delays_silent_sockets_controls_and_sanitization(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            held = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
            try:
                for sock in held:
                    sock.bind(('127.0.0.1', 0))
                ports = [sock.getsockname()[1] for sock in held]
            finally:
                for sock in held:
                    sock.close()
            server_log = temp/'server.jsonl'
            server = subprocess.Popen([sys.executable, str(ROOT/'idle-matrix.py'),
                                       'server', '127.0.0.1', ','.join(map(str, ports)),
                                       '--seconds', '2', '--private-log', str(server_log)],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic()+3
                while not server_log.exists() or not server_log.read_text():
                    if server.poll() is not None or time.monotonic() > deadline:
                        self.fail('server did not become ready')
                    time.sleep(.01)
                client_log = temp/'client.jsonl'
                with client_log.open('w') as log:
                    result = matrix.probe('127.0.0.1', ports, [.08, .2], 1, .15, 25, .01, log)
                self.assertEqual(len(result['trials']), 8)
                for trial in result['trials']:
                    self.assertEqual(set(trial['legs']), {'delayed', 'control_start', 'control_end'})
                    for leg in trial['legs'].values():
                        self.assertTrue(leg['received'])
                    self.assertGreaterEqual(trial['legs']['delayed']['elapsed_ms'],
                                            trial['delay_seconds']*1000)
                client = [json.loads(v) for v in client_log.read_text().splitlines()]
                sends = [v for v in client if v['event'] == 'sent']
                # Not even a control socket reuses a silent socket's local port.
                self.assertEqual(len({v['local_port'] for v in sends}), 24)
                server.wait(timeout=4)
                self.assertEqual(server.returncode, 0, server.stderr.read().decode())
                raw = [json.loads(v) for v in server_log.read_text().splitlines()]
                self.assertEqual(len([v for v in raw if v['event'] == 'received']), 24)
                self.assertEqual(len([v for v in raw if v['event'] == 'sent']), 24)
                for sent in [v for v in raw if v['event'] == 'sent']:
                    req = next(v for v in sends if v['token'] == sent['token'])
                    self.assertGreaterEqual(sent['server_wait_ms'], req['delay_seconds']*1000)
                self.assertEqual(server_log.stat().st_mode & 0o777, 0o600)
                joined = evidence.analyze(client_log, server_log)
                self.assertTrue(joined['client_completed'])
                self.assertEqual(joined['server_pending_replies'], [0])
                self.assertTrue(all(v['server_received'] and v['server_sent'] and v['client_received']
                                    for v in joined['legs']))
                for output in (result, joined):
                    encoded = json.dumps(output)
                    for private in ('127.0.0.1', 'observed_peer', 'token', 'local_port', 'fingerprint'):
                        self.assertNotIn(private, encoded)
            finally:
                if server.poll() is None:
                    server.terminate()
                    server.wait(timeout=3)
                server.stderr.close()

    def test_evidence_distinguishes_send_capture_and_delivery(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            request, reply = matrix.wire({'request': 1}), matrix.wire({'reply': 1})
            tx = dict(event='sent', token='private', trial=0, leg='delayed', destination=0,
                      source=1, delay_seconds=30, fingerprint=matrix.fingerprint(request))
            client_log, server_log = temp/'client', temp/'server'
            client_log.write_text(json.dumps(tx)+'\n')
            server_log.write_text('\n'.join(map(json.dumps, [
                dict(event='received', token='private', observed_peer=['192.0.2.1', 12345]),
                dict(event='sent', token='private', fingerprint=matrix.fingerprint(reply), server_wait_ms=30000)])))
            for link in (1, 101, 113, 276):
                capture = temp/'capture.pcap'
                pcap(capture, [request, reply, b'unrelated'], link)
                joined = evidence.analyze(client_log, server_log, server_pcap=capture)
                row = joined['legs'][0]
                self.assertTrue(row['server_sent'])
                self.assertFalse(row['client_received'])
                self.assertIsNone(row['client_capture_reply_packets'])
                self.assertEqual(row['server_capture_reply_packets'], 1)
                # Removing a send event must not turn missing evidence into a
                # zero captured reply: there is no known fingerprint to match.
            server_log.write_text('')
            joined = evidence.analyze(client_log, server_log, server_pcap=capture)
            self.assertFalse(joined['legs'][0]['server_received'])
            self.assertIsNone(joined['legs'][0]['server_capture_reply_packets'])
            capture.write_bytes(capture.read_bytes()[:-1])
            with self.assertRaisesRegex(ValueError, 'truncated'):
                evidence.capture_counts(capture, set())

    def test_missing_replies_remain_failures(self):
        # A bound socket with no responder yields negative results, never a
        # success just because the scheduler completed its deadlines.
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as one, \
                socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as two:
            one.bind(('127.0.0.1', 0))
            two.bind(('127.0.0.1', 0))
            result = matrix.probe('127.0.0.1', [one.getsockname()[1], two.getsockname()[1]],
                                  [.05], 1, .05, 1, 0, io.StringIO())
            self.assertTrue(all(not leg['received'] for row in result['trials']
                                for leg in row['legs'].values()))
            self.assertEqual(result['mapping_count'], 0)

    def test_bounds_and_exclusive_private_log(self):
        with tempfile.TemporaryDirectory() as temp:
            log = pathlib.Path(temp)/'private'
            log.write_text('preserve')
            base = [sys.executable, str(ROOT/'idle-matrix.py'), 'client', '127.0.0.1',
                    '1234,2345', '--private-log', str(log)]
            for options in (['--delays', 'nan'], ['--delays', 'inf'], ['--delays', '0'],
                            ['--delays', '1,1'], ['--rounds', '0'], ['--window', 'nan'],
                            ['--launch-gap', '-1'], ['--rounds', '5', '--delays', '1,2,3,4,5,6,7,8']):
                outcome = subprocess.run(base+options, capture_output=True)
                self.assertEqual(outcome.returncode, 2)
            outcome = subprocess.run(base+['--delays', '.05'], capture_output=True)
            self.assertNotEqual(outcome.returncode, 0)
            self.assertEqual(log.read_text(), 'preserve')


if __name__ == '__main__':
    unittest.main()
