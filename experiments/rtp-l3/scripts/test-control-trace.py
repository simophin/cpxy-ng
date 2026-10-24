#!/usr/bin/env python3
"""Deterministic diagnosis fixtures; no network or identifying artifacts."""
import hashlib
import importlib.util
import pathlib
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('trace', pathlib.Path(__file__).with_name('control-trace.py'))
trace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trace)


def event(stage, packet='a', clock=100, **fields):
    return dict(stage=stage, packet=packet, unix_us=clock, mono_us=clock,
                port=4000, **fields)


def send(kind, packet='a', clock=100, sent=True):
    return event('send', packet, clock, sent=sent,
                 control=dict(kind=kind, session=999, seq=1))


class Correlation(unittest.TestCase):
    def test_loss_superseded_echo_completion_duplicates_and_clock_bounds(self):
        client = [send('ping'), event('receive', 'b', 140), event('receive', 'b', 141),
                  event('decode', 'b', 142, result='control'),
                  event('state', 'b', 143, result='echo_not_pending', pending=2),
                  event('receive', 'c', 160), event('state', 'c', 161, result='accepted'),
                  send('ping', 'lost'), send('ping', 'failed', sent=False)]
        server = [event('receive', clock=120), event('decode', result='control'),
                  event('ping', result='accepted'), send('state', 'b', 130),
                  send('state', 'c', 150), send('state', 'missing', 170)]
        result = trace.summarize(client, server, {'a': 1, 'missing': 1})
        up = result['directions']['client_to_server']
        down = result['directions']['server_to_client']
        self.assertEqual(up['ports']['port-1']['send_failed'], 1)
        self.assertEqual(up['ports']['port-1']['missing_receive'], 1)
        self.assertEqual(down['ports']['port-1']['duplicate_receives'], 1)
        self.assertEqual(down['ports']['port-1']['state:accepted'], 1)
        self.assertEqual(down['ignored_states'][0]['pending'], 2)
        self.assertTrue(down['missing_receive'][0]['server_capture_seen'])
        self.assertNotIn('999', str(result))
        self.assertEqual(result['clock_offset_server_minus_client_us']['lower'], -10)
        self.assertEqual(result['clock_offset_server_minus_client_us']['upper'], 20)

    def test_fresh_ping_to_reply_lifecycle(self):
        reply = send('state', 'reply', 130)
        reply['control'] = dict(kind='state', session=999, echo=1)
        client = [send('ping'), event('receive', 'reply', 140),
                  event('decode', 'reply', 141, result='control'),
                  event('state', 'reply', 142, result='accepted')]
        result = trace.summarize(client, [event('ping', result='accepted'), reply], {'reply': 1})
        lifecycle = result['ping_lifecycles'][0]
        self.assertEqual(lifecycle['server_ping_result'], 'accepted')
        self.assertEqual(lifecycle['replies'][0]['state'], ['accepted'])
        self.assertTrue(lifecycle['replies'][0]['server_capture_seen'])
        self.assertNotIn('999', str(result))

    def test_decode_rejection_and_empty_trace(self):
        result = trace.summarize([send('ping')],
                                 [event('receive'), event('decode', result='rejected')])
        self.assertEqual(result['directions']['client_to_server']['ports']['port-1']['decode:rejected'], 1)
        with self.assertRaises(ValueError):
            trace.summarize([], [])

    def test_pcap_payload_length_padding_and_truncation(self):
        payload = b'fingerprint'
        ip = bytearray(20)
        ip[0] = 0x45
        ip[9] = 17
        ip[2:4] = struct.pack('!H', 28 + len(payload))
        udp = struct.pack('!HHHH', 4000, 4001, 8 + len(payload), 0)
        packet = bytes(12) + b'\x08\x00' + ip + udp + payload + bytes(8)
        header = struct.pack('<IHHIIII', 0xa1b2c3d4, 2, 4, 0, 0, 65535, 1)
        record = struct.pack('<IIII', 1, 0, len(packet), len(packet))
        with tempfile.NamedTemporaryFile() as f:
            f.write(header + record + packet); f.flush()
            self.assertEqual(trace.capture_fingerprints(f.name), {hashlib.sha256(payload).hexdigest(): 1})
            f.seek(0); f.truncate(); f.write(header + record + packet[:-1]); f.flush()
            with self.assertRaises(ValueError):
                trace.capture_fingerprints(f.name)


if __name__ == '__main__':
    unittest.main()
