#!/usr/bin/env python3
"""Plaintext no-auth SOCKS5 server for the isolated router namespace fixture only."""
import selectors
import socket
import socketserver
import struct
import sys


def read_exact(stream, length):
    data = b""
    while len(data) < length:
        chunk = stream.recv(length - len(data))
        if not chunk:
            raise EOFError
        data += chunk
    return data


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            client = self.request
            version, length = read_exact(client, 2)
            assert version == 5 and 0 in read_exact(client, length)
            client.sendall(b"\x05\x00")
            version, command, reserved, kind = read_exact(client, 4)
            assert (version, command, reserved) == (5, 1, 0)
            if kind in (1, 4):
                family, length = (socket.AF_INET, 4) if kind == 1 else (socket.AF_INET6, 16)
                host = socket.inet_ntop(family, read_exact(client, length))
            else:
                assert kind == 3
                host = read_exact(client, read_exact(client, 1)[0]).decode("ascii")
            port = struct.unpack("!H", read_exact(client, 2))[0]
            print(f"CONNECT {host} {port}", flush=True)
            with socket.create_connection((host, port), timeout=5) as upstream:
                client.sendall(b"\x05\x00\x00\x01" + b"\x00" * 6)
                upstream.settimeout(None)
                with selectors.DefaultSelector() as selector:
                    selector.register(client, selectors.EVENT_READ, upstream)
                    selector.register(upstream, selectors.EVENT_READ, client)
                    while selector.get_map():
                        for key, _ in selector.select(timeout=10):
                            data = key.fileobj.recv(65536)
                            if data:
                                key.data.sendall(data)
                            else:
                                selector.unregister(key.fileobj)
                                key.data.shutdown(socket.SHUT_WR)
        except (OSError, EOFError):
            pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    Server.address_family = socket.AF_INET6 if ":" in sys.argv[1] else socket.AF_INET
    with Server((sys.argv[1], int(sys.argv[2])), Handler) as server:
        server.serve_forever()
