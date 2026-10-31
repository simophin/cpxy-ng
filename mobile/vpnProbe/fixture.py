#!/usr/bin/env python3
"""Loopback-only host fixtures for the separate-UID Android VPN smoke tests."""
import http.server
import importlib.util
from pathlib import Path
import socketserver
import sys
import threading

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "socks_fixture", root / "packaging/openwrt/test/socks5-fixture.py"
)
socks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(socks)


class Http(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"fixture\n")

    def log_message(self, *args):
        pass


class Udp(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        sock.sendto(data, self.client_address)


servers = [
    socks.Server(("127.0.0.1", 11080), socks.Handler),
    http.server.ThreadingHTTPServer(("127.0.0.1", 11081), Http),
    socketserver.ThreadingUDPServer(("127.0.0.1", 11082), Udp),
]
try:
    for server in servers:
        threading.Thread(target=server.serve_forever, daemon=True).start()
    threading.Event().wait()
except KeyboardInterrupt:
    pass
finally:
    for server in servers:
        server.shutdown()
        server.server_close()
