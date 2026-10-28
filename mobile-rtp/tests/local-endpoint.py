#!/usr/bin/env python3
"""Disposable emulator endpoint. Run as root; only loopback ports and private namespaces change.

sudo python3 mobile-rtp/tests/local-endpoint.py /tmp/rtp-android-fixture
The emulator connects to 10.0.2.2:33000. Fixture password: android-rtp-test-password.
Stop with SIGINT; no public listener, host route, NAT or firewall changes are made.
"""
import http.server
import os
from pathlib import Path
import selectors
import signal
import socket
import struct
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
PORTS = [*range(33001, 33017), 33053]
PASSWORD = "android-rtp-test-password"


def command(*args):
    subprocess.run(args, check=True)


class Echo(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = (self.client_address[0] + "\n").encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def http_echo(host, port):
    server = http.server.ThreadingHTTPServer((host, port), Echo)
    threading.Thread(target=server.serve_forever, daemon=True).start()


def namespace(work, channel_fd):
    command("mount", "--make-rprivate", "/")
    command("mount", "-t", "tmpfs", "tmpfs", "/run")
    command("ip", "link", "set", "lo", "up")
    for ip in ("203.0.113.2", "8.8.8.8", "223.5.5.5"):
        command("ip", "addr", "add", ip + "/32", "dev", "lo")
    Path("/proc/sys/net/ipv4/ip_forward").write_text("1")
    children = []
    log = open(work / "endpoint.log", "ab", buffering=0)
    env = dict(os.environ, CPXY_RTP_PASSWORD=PASSWORD)
    endpoint_argv = [
        str(ROOT / "target/debug/cpxy-rtp"), "endpoint", "--internet-exit",
        "--listen", "127.0.0.1:33000", "--udp-ports", "33001-33016",
        "--firewall", "none", "--state-dir", str(work / "state"),
    ]
    pool_low = 2
    endpoint = subprocess.Popen(endpoint_argv, env=env, stdout=log, stderr=log)
    children.append(endpoint)
    (work / "endpoint.pid").write_text(str(endpoint.pid))
    http_echo("203.0.113.2", 8080)
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("203.0.113.2", 8080))

    def echo_udp():
        while True:
            data, peer = udp.recvfrom(2048)
            udp.sendto(peer[0].encode(), peer)

    threading.Thread(target=echo_udp, daemon=True).start()
    for ip in ("8.8.8.8", "223.5.5.5"):
        children.append(subprocess.Popen([
            "python3", str(ROOT / "mobile-engine/test/dns.py"), "serve", ip, "0",
            "remote.test=203.0.113.2", "local.test=223.5.5.5",
        ], stdout=log, stderr=log))
    channel = socket.socket(fileno=channel_fd)
    channel.setblocking(False)
    poll = selectors.DefaultSelector()
    poll.register(channel, selectors.EVENT_READ, None)
    sockets = {}
    for port in PORTS:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.connect(("223.5.5.5", 53) if port == 33053 else ("127.0.0.1", port))
        sock.setblocking(False)
        sockets[port] = sock
        poll.register(sock, selectors.EVENT_READ, port)
    (work / "ready").write_text(str(os.getpid()))
    try:
        while True:
            if (work / "command").exists():
                action = (work / "command").read_text()
                (work / "command").unlink()
                endpoint.send_signal(signal.SIGCONT)
                endpoint.terminate()
                endpoint.wait(timeout=10)
                argv = endpoint_argv.copy()
                if action == "reassign":
                    argv[argv.index("--state-dir") + 1] = str(work / ("replacement-" + str(time.monotonic_ns())))
                    pool_low = 128 if pool_low == 2 else 2
                    if "--pool-low" in argv:
                        argv[argv.index("--pool-low") + 1] = "10.88.0." + str(pool_low)
                    else:
                        argv += ["--pool-low", "10.88.0." + str(pool_low)]
                endpoint_argv = argv
                endpoint = subprocess.Popen(argv, env=env, stdout=log, stderr=log)
                children.append(endpoint)
                (work / "endpoint.pid").write_text(str(endpoint.pid))
                deadline = time.monotonic() + 10
                while True:
                    try:
                        with socket.create_connection(("127.0.0.1", 33000), timeout=.1):
                            break
                    except OSError:
                        if endpoint.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError("Endpoint failed to restart")
                        time.sleep(.1)
                (work / "command-done").touch()
            for key, _ in poll.select(1):
                try:
                    if key.data is None:
                        msg = channel.recv(2048)
                        if not msg:
                            return
                        sockets[struct.unpack("!H", msg[:2])[0]].send(msg[2:])
                    else:
                        channel.send(struct.pack("!H", key.data) + key.fileobj.recv(2048))
                except (ConnectionRefusedError, BlockingIOError):
                    # A send can consume an ICMP error after select marked the same socket
                    # readable. Never block recv on that stale readiness indication.
                    pass
    finally:
        for process in children:
            if process.poll() is None:
                process.send_signal(signal.SIGCONT)
                process.terminate()
        for process in children:
            process.wait(timeout=10)


def tcp_relay(fd):
    external = socket.socket(fileno=fd)
    internal = socket.create_connection(("127.0.0.1", 33000), timeout=5)
    with external, internal, selectors.DefaultSelector() as poll:
        poll.register(external, selectors.EVENT_READ, internal)
        poll.register(internal, selectors.EVENT_READ, external)
        while True:
            events = poll.select(6)
            if not events:
                return
            for key, _ in events:
                data = key.fileobj.recv(32768)
                if not data:
                    return
                key.data.sendall(data)


def host(work):
    if os.geteuid() != 0:
        raise SystemExit("Run with sudo; all network mutations are inside unshare -nm")
    work.mkdir(mode=0o755, parents=True, exist_ok=False)
    channel, child = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    channel.setblocking(False)
    process = subprocess.Popen([
        "unshare", "-nm", "python3", __file__, "namespace", str(work), str(child.fileno()),
    ], pass_fds=(child.fileno(),), start_new_session=True)
    child.close()
    try:
        deadline = time.monotonic() + 20
        while not (work / "ready").exists():
            if process.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError("Namespace endpoint did not start")
            time.sleep(.1)
        (work / "namespace.pid").write_text(str(process.pid))
        peers = {}
        sockets = {}
        poll = selectors.DefaultSelector()
        poll.register(channel, selectors.EVENT_READ, None)
        for port in PORTS:
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.bind(("127.0.0.1", port))
            sock.setblocking(False)
            sockets[port] = sock
            poll.register(sock, selectors.EVENT_READ, port)
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 33000))
        listener.listen()
        listener.setblocking(False)
        poll.register(listener, selectors.EVENT_READ, "tcp")
        http_echo("127.0.0.1", 33080)
        class Control(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path in ("/pause", "/resume"):
                    pid = int((work / "endpoint.pid").read_text())
                    os.kill(pid, signal.SIGSTOP if self.path == "/pause" else signal.SIGCONT)
                elif self.path in ("/restart", "/reassign"):
                    (work / "command-done").unlink(missing_ok=True)
                    (work / "command").write_text(self.path[1:])
                    deadline = time.monotonic() + 20
                    while not (work / "command-done").exists():
                        if time.monotonic() > deadline:
                            self.send_error(500)
                            return
                        time.sleep(.1)
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", "3")
                self.end_headers()
                self.wfile.write(b"OK\n")

            def log_message(self, *_):
                pass

        control = http.server.HTTPServer(("127.0.0.1", 33090), Control)
        threading.Thread(target=control.serve_forever, daemon=True).start()
        print("Local endpoint ready: emulator 10.0.2.2:33000", flush=True)
        while True:
            for key, _ in poll.select():
                try:
                    if key.data == "tcp":
                        connection, _ = listener.accept()
                        relay = subprocess.Popen([
                            "nsenter", "-t", str(process.pid), "-n", "python3", __file__, "tcp", str(connection.fileno()),
                        ], pass_fds=(connection.fileno(),))
                        connection.close()
                        threading.Thread(target=relay.wait, daemon=True).start()
                    elif key.data is None:
                        msg = channel.recv(2048)
                        if not msg:
                            return
                        port = struct.unpack("!H", msg[:2])[0]
                        if port in peers:
                            sockets[port].sendto(msg[2:], peers[port])
                    else:
                        data, peer = key.fileobj.recvfrom(2048)
                        peers[key.data] = peer
                        channel.send(struct.pack("!H", key.data) + data)
                except BlockingIOError:
                    pass
    finally:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=10)
        channel.close()


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    if sys.argv[1] == "namespace":
        namespace(Path(sys.argv[2]), int(sys.argv[3]))
    elif sys.argv[1] == "tcp":
        tcp_relay(int(sys.argv[2]))
    else:
        try:
            host(Path(sys.argv[1]).resolve())
        except KeyboardInterrupt:
            pass
