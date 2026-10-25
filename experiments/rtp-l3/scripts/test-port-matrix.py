#!/usr/bin/env python3
"""Exercise real UDP correlation, mapping sanitization and CLI bounds."""
import importlib.util
import json
import pathlib
import socket
import subprocess
import sys
import unittest

PATH = pathlib.Path(__file__).with_name('port-matrix.py')
spec = importlib.util.spec_from_file_location('port_matrix', PATH)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class MatrixTest(unittest.TestCase):
    def test_local_delivery_and_sanitization(self):
        held = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(3)]
        try:
            for sock in held:
                sock.bind(('127.0.0.1', 0))
            ports = [sock.getsockname()[1] for sock in held]
        finally:
            for sock in held:
                sock.close()
        server = subprocess.Popen([sys.executable, str(PATH), 'server', '127.0.0.1',
                                   ','.join(map(str, ports)), '--seconds', '5'],
                                  stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(json.loads(server.stdout.readline())['event'], 'ready')
            result = module.probe('127.0.0.1', ports, 2, .05, 7)
            self.assertEqual(result['mapping_count'], 1)
            self.assertEqual(len(result['trials']), 6)
            for row in result['trials']:
                self.assertEqual({v['source'] for v in row['replies']}, {0, 1, 2})
            encoded = json.dumps(result)
            self.assertNotIn('127.0.0.1', encoded)
            self.assertNotIn('observed_peer', encoded)
            self.assertNotIn('token', encoded)
            for port in ports:
                self.assertNotIn(str(port), encoded)
        finally:
            server.terminate()
            server.wait(timeout=3)
            server.stdout.close()

    def test_bounds(self):
        for ports in ['1234,1234', '1234', '1234,nope']:
            result = subprocess.run([sys.executable, str(PATH), 'client', '127.0.0.1', ports],
                                    capture_output=True)
            self.assertEqual(result.returncode, 2)


if __name__ == '__main__':
    unittest.main()
