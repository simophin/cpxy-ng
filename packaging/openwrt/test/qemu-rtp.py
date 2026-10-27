#!/usr/bin/env python3
"""Disposable OpenWrt RTP integration.
Run via private-linux-lab.sh and sudo unshare -nm, see rtp-router.md.
Arguments: IMAGE IPK DEPS_DIR ENDPOINT_BINARY RESULTS_DIR.
"""
import contextlib
import http.server
import os
from pathlib import Path
import re
import selectors
import shlex
import socket
import struct
import subprocess
import sys
import threading
import time


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout.strip()


def ns(name, *args):
    return run('ip', 'netns', 'exec', name, *args)


def fixture():
    """Controlled DNS and HTTP endpoints; runs in the WAN namespace."""
    import threading
    import socketserver

    results = Path(sys.argv[2])

    def capture():
        with socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3)) as sock, \
                open(results / 'wan.pcap', 'wb', buffering=0) as out:
            sock.bind(('nic', 0))
            out.write(struct.pack('<IHHIIII', 0xa1b2c3d4, 2, 4, 0, 0, 65535, 1))
            while True:
                frame = sock.recv(65535)
                now = time.time()
                out.write(struct.pack('<IIII', int(now), int(now % 1 * 1e6), len(frame), len(frame)) + frame)
                if len(frame) < 42 or frame[12:14] != b'\x08\x00':
                    continue
                ip = frame[14:]
                offset = (ip[0] & 15) * 4
                if (ip[9] in (6, 17) and ip[12:16] == socket.inet_aton('192.0.2.2')
                        and ip[16:20] == socket.inet_aton('93.184.216.34')
                        and struct.unpack('!H', ip[offset+2:offset+4])[0] == 8000
                        and (results / 'enforce').exists()):
                    (results / 'leak').write_text('Non-regional TCP/UDP escaped directly to WAN')

    threading.Thread(target=capture, daemon=True).start()

    class Web(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = self.client_address[0].encode()
            self.send_response(200)
            self.end_headers()
            self.wfile.write(body)

    def answer(query):
        name = query[12:].split(b'\x00', 1)[0]
        ip = '114.114.114.114' if b'regional' in name else '93.184.216.34'
        # Preserve the question; return one A record, or an empty answer for other types.
        end = query.index(b'\x00', 12) + 5
        a = query[end-4:end-2] == b'\x00\x01'
        return (query[:2] + struct.pack('!5H', 0x8180, 1, int(a), 0, 0) + query[12:end]
                + (b'\xc0\x0c' + struct.pack('!HHIH', 1, 1, 60, 4) + socket.inet_aton(ip) if a else b''))

    class UDP(socketserver.BaseRequestHandler):
        def handle(self):
            query, sock = self.request
            print(f'DNS UDP peer={self.client_address[0]}', flush=True)
            sock.sendto(answer(query), self.client_address)

    class Echo(socketserver.BaseRequestHandler):
        def handle(self):
            data, sock = self.request
            sock.sendto(self.client_address[0].encode(), self.client_address)

    class TCP(socketserver.BaseRequestHandler):
        def handle(self):
            print(f'DNS TCP peer={self.client_address[0]}', flush=True)
            f = self.request.makefile('rb')
            while header := f.read(2):
                data = answer(f.read(struct.unpack('!H', header)[0]))
                self.request.sendall(struct.pack('!H', len(data)) + data)

    class V6Web(http.server.ThreadingHTTPServer):
        address_family = socket.AF_INET6

    for server in [socketserver.ThreadingUDPServer(('114.114.114.114', 8000), Echo),
                   socketserver.ThreadingUDPServer(('93.184.216.34', 8000), Echo),
                   socketserver.ThreadingUDPServer(('8.8.8.8', 53), UDP),
                   socketserver.ThreadingUDPServer(('192.0.2.1', 53), UDP),
                   socketserver.ThreadingTCPServer(('192.0.2.1', 53), TCP),
                   http.server.ThreadingHTTPServer(('0.0.0.0', 8000), Web),
                   V6Web(('2001:db8:100::34', 8000), Web)]:
        threading.Thread(target=server.serve_forever, daemon=True).start()
    while True:
        time.sleep(60)


class Console:
    def __init__(self, process, log):
        self.process, self.log, self.buffer = process, log, b''
        self.selector = selectors.DefaultSelector()
        self.selector.register(process.stdout, selectors.EVENT_READ)
        self.sequence = 0

    def until(self, pattern, timeout=90):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            match = re.search(pattern, self.buffer)
            if match:
                before = self.buffer[:match.start()]
                self.buffer = self.buffer[match.end():]
                return before.decode(errors='replace'), match
            if self.selector.select(1):
                data = os.read(self.process.stdout.fileno(), 65536)
                if not data:
                    raise RuntimeError('QEMU serial console closed')
                self.log.write(data)
                self.log.flush()
                self.buffer += data
        raise TimeoutError(f'Console timeout waiting for {pattern!r}: {self.buffer[-2000:]!r}')

    def send(self, command):
        self.process.stdin.write(command.encode() + b'\n')
        self.process.stdin.flush()

    def command(self, command, timeout=90):
        if len(command) > 350:
            self.command('true > /tmp/rtp-console.sh')
            for start in range(0, len(command), 100):
                self.command("printf '%s' " + shlex.quote(command[start:start+100]) + " >> /tmp/rtp-console.sh")
            return self.command('sh /tmp/rtp-console.sh', timeout)
        self.sequence += 1
        marker = f'__CPXY_{self.sequence}'
        self.send(command + f"; printf '\\n{marker}:%s\\n' \"$?\"")
        output, match = self.until(marker.encode() + rb':([0-9]+)\r?\n', timeout)
        assert match[1] == b'0', f'Guest command failed: {command}\n{output}'
        return output


def main():
    image, package, deps, server, results = map(lambda p: Path(p).resolve(), sys.argv[1:])
    if os.getuid() != 0 or any(os.readlink('/proc/self/ns/' + n) == os.readlink('/proc/1/ns/' + n) for n in ['net', 'mnt']):
        raise SystemExit('Private mount/network namespaces required')
    results.mkdir(parents=True, exist_ok=True)
    # The caller must use an isolated user/mount/network namespace.
    run('mount', '--make-rprivate', '/')
    run('mount', '-t', 'tmpfs', 'tmpfs', '/run')
    Path('/run/netns').mkdir()
    for name in ['inet', 'lan', 'guest', 'upstream']:
        run('ip', 'netns', 'add', name)
        ns(name, 'ip', 'link', 'set', 'lo', 'up')
    for index, name in enumerate(['lan', 'inet', 'guest']):
        bridge, tap, peer = f'br{index}', f'tap{index}', f'peer{index}'
        run('ip', 'link', 'add', bridge, 'type', 'bridge')
        run('ip', 'link', 'set', bridge, 'up')
        run('ip', 'tuntap', 'add', tap, 'mode', 'tap')
        run('ip', 'link', 'set', tap, 'master', bridge)
        run('ip', 'link', 'set', tap, 'up')
        run('ip', 'link', 'add', peer, 'type', 'veth', 'peer', 'name', 'nic')
        run('ip', 'link', 'set', 'nic', 'netns', name)
        run('ip', 'link', 'set', peer, 'master', bridge)
        run('ip', 'link', 'set', peer, 'up')
        ns(name, 'ip', 'link', 'set', 'nic', 'up')
    for name, addr, gateway in [('lan', '192.168.8.50/24', '192.168.8.1'),
                                ('guest', '192.168.9.50/24', '192.168.9.1'),
                                ('inet', '192.0.2.1/24', None)]:
        ns(name, 'ip', 'addr', 'add', addr, 'dev', 'nic')
        if gateway:
            ns(name, 'ip', 'route', 'add', 'default', 'via', gateway)
    for addr in ['93.184.216.34/32', '114.114.114.114/32', '8.8.8.8/32']:
        ns('inet', 'ip', 'addr', 'add', addr, 'dev', 'lo')
    run('ip', 'link', 'add', 'uppeer', 'type', 'veth', 'peer', 'name', 'upnic')
    run('ip', 'link', 'set', 'upnic', 'netns', 'upstream')
    run('ip', 'link', 'set', 'uppeer', 'master', 'br1')
    run('ip', 'link', 'set', 'uppeer', 'up')
    ns('upstream', 'ip', 'addr', 'add', '192.0.2.3/24', 'dev', 'upnic')
    ns('upstream', 'ip', 'link', 'set', 'upnic', 'up')
    ns('upstream', 'ip', 'route', 'add', 'default', 'via', '192.0.2.1')
    ns('inet', 'sysctl', '-qw', 'net.ipv4.ip_forward=1')
    ns('upstream', 'sysctl', '-qw', 'net.ipv4.ip_forward=1')
    ns('inet', 'ip', '-6', 'addr', 'add', '2001:db8:100::34/128', 'dev', 'lo', 'nodad')
    ns('inet', 'ip', '-6', 'addr', 'add', '2001:db8:2::1/64', 'dev', 'nic', 'nodad')
    ns('inet', 'ip', '-6', 'route', 'add', '2001:db8:8::/64', 'via', '2001:db8:2::2')
    ns('lan', 'ip', '-6', 'addr', 'add', '2001:db8:8::50/64', 'dev', 'nic', 'nodad')
    ns('lan', 'ip', '-6', 'route', 'add', 'default', 'via', '2001:db8:8::1')
    ns('inet', 'ip', 'route', 'add', '10.88.0.0/24', 'via', '192.0.2.3')
    processes = []
    stop_probes = threading.Event()
    probe_thread = None
    with contextlib.ExitStack() as stack:
        def background(args, log):
            handle = stack.enter_context(open(results / log, 'wb'))
            process = subprocess.Popen(args, stdout=handle, stderr=subprocess.STDOUT)
            processes.append(process)
            return process

        try:
            for name in ['enforce', 'leak']:
                (results / name).unlink(missing_ok=True)
            background(['ip', 'netns', 'exec', 'inet', sys.executable, __file__, '--fixture', str(results)], 'fixtures.log')
            import secrets
            password = secrets.token_hex(24)
            os.environ['CPXY_RTP_PASSWORD'] = password
            endpoint = background(['ip', 'netns', 'exec', 'upstream', str(server), 'endpoint', '--listen', '192.0.2.3:4000', '--internet-exit', '--interface', 'rtpe', '--firewall', 'none', '--state-dir', str(results / 'endpoint-state')], 'server.log')
            # Serve only test assets; never expose the repository or credentials.
            assets = results / 'assets'
            assets.mkdir(exist_ok=True)
            import shutil
            shutil.copy(package, assets / 'cpxy-rtp.ipk')
            for dep in deps.glob('*.ipk'):
                shutil.copy(dep, assets / dep.name)
            background(['ip', 'netns', 'exec', 'inet', sys.executable, '-m', 'http.server', '8080', '--directory', str(assets)], 'assets.log')
            args = ['qemu-system-x86_64', '-m', '512', '-smp', '2', '-display', 'none',
                    '-monitor', 'none', '-serial', 'stdio', '-snapshot',
                    '-drive', f'file={image},format=raw,if=virtio']
            for index in range(3):
                args += ['-netdev', f'tap,id=n{index},ifname=tap{index},script=no,downscript=no',
                         '-device', f'e1000,netdev=n{index},mac=52:54:00:00:00:0{index+1}']
            vm = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            processes.append(vm)
            console = Console(vm, stack.enter_context(open(results / 'serial.log', 'wb')))
            console.until(rb'Please press Enter to activate this console', 120)
            console.send('')
            console.until(rb'root@[^\r\n]*#')
            console.command("for n in $(seq 1 90); do ubus list network.interface.lan | grep -q network.interface.lan && break; sleep 1; done; test -f /etc/config/network", 120)
            console.command("uci set network.lan.ipaddr=192.168.8.1; uci set network.wan.proto=static; uci set network.wan.ipaddr=192.0.2.2; uci set network.wan.netmask=255.255.255.0; uci set network.wan.gateway=192.0.2.1; uci commit network; /etc/init.d/network restart")
            console.command("sh -c 'for n in $(seq 1 60); do ping -c1 -W1 192.0.2.1 >/dev/null 2>&1 && exit 0; sleep 1; done; exit 1'", 120)
            for name in [p.name for p in assets.glob('*.ipk')]:
                console.command(f'wget -q http://192.0.2.1:8080/{shlex.quote(name)} -O /tmp/{shlex.quote(name)}')
            console.command('opkg install /tmp/*.ipk', 120)
            console.command("uci set network.guest=interface; uci set network.guest.device=eth2; uci set network.guest.proto=static; uci set network.guest.ipaddr=192.168.9.1; uci set network.guest.netmask=255.255.255.0; uci commit network; /etc/init.d/network reload; sleep 3")
            console.command("uci set firewall.guests=zone; uci set firewall.guests.name=guests; uci add_list firewall.guests.network=guest; uci set firewall.guests.input=ACCEPT; uci set firewall.guests.output=ACCEPT; uci set firewall.guests.forward=REJECT; uci set firewall.guestwan=forwarding; uci set firewall.guestwan.src=guests; uci set firewall.guestwan.dest=wan; uci commit firewall; /etc/init.d/firewall reload")
            console.command('test ! -e /var/run/cpxy-rtp/enabled; test ! -e /etc/config/cpxy')
            # A retained router configuration must survive all RTP operations unchanged.
            console.command("printf \"config defaults 'defaults'\\n option router '1'\\nconfig instance 'main'\\n option enabled '1'\\n list network 'lan'\\n\" >/etc/config/cpxy; sha256sum /etc/config/cpxy >/etc/old-config.sha")
            console.command("uci set cpxy_rtp.main.entry=192.0.2.3:4000; uci set cpxy_rtp.main.password='" + password + "'; uci set cpxy_rtp.main.enabled=1; uci -q delete cpxy_rtp.main.network; uci add_list cpxy_rtp.main.network=guest; uci -q delete cpxy_rtp.main.dns_upstream; uci add_list cpxy_rtp.main.dns_upstream=192.0.2.1; uci -q delete cpxy_rtp.main.dns_alternative; uci add_list cpxy_rtp.main.dns_alternative=8.8.8.8; uci commit cpxy_rtp; /etc/init.d/cpxy-rtp-router enable; /etc/init.d/cpxy-rtp-router start", 150)

            def fetch(ip, source='guest'):
                return ns(source, 'curl', '-fsS', '--noproxy', '*', '--max-time', '5', f'http://{ip}:8000/')

            def udp(ip):
                return ns('guest', sys.executable, '-c', "import socket,sys;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(3);s.sendto(b'fixture',(sys.argv[1],8000));print(s.recv(100).decode())", ip)

            def paths():
                assert fetch('114.114.114.114') == '192.0.2.2'
                assert fetch('93.184.216.34') == '10.88.0.2'
                assert udp('114.114.114.114') == '192.0.2.2'
                assert udp('93.184.216.34') == '10.88.0.2'

            def wait_paths():
                deadline = time.monotonic() + 60
                while True:
                    try:
                        paths()
                        return
                    except (subprocess.CalledProcessError, AssertionError):
                        if time.monotonic() > deadline:
                            raise
                        time.sleep(1)

            console.command('sleep 5; ip -4 rule show; ip addr show rtpr0; ls -la /var/run/cpxy-rtp; ubus call service list; logread | tail -30')
            wait_paths()
            assert fetch('93.184.216.34', 'lan') == '192.0.2.2'
            print('PASS: regional TCP/UDP direct, nonregional TCP/UDP RTP, unselected LAN unchanged', flush=True)
            # Capture is armed only after the unselected LAN baseline probe.
            (results / 'enforce').touch()
            def probe():
                while not stop_probes.is_set():
                    try:
                        source = ns('guest', 'curl', '-fsS', '--noproxy', '*', '--max-time', '1', 'http://93.184.216.34:8000/')
                        if source != '10.88.0.2':
                            (results / 'leak').write_text('Continuous selected-LAN probe bypassed RTP')
                    except subprocess.CalledProcessError:
                        pass
                    stop_probes.wait(0.2)
            probe_thread = threading.Thread(target=probe, daemon=True)
            probe_thread.start()
            dns_client = Path(__file__).resolve().parents[3] / 'mobile-engine/test/dns.py'
            for transport in ['udp', 'tcp']:
                for name, expected in [('regional.test', '114.114.114.114'), ('proxy.test', '93.184.216.34')]:
                    assert ns('guest', sys.executable, str(dns_client), 'query', '192.168.9.1', name, transport, 'A') == 'NOERROR ' + expected
                assert ns('guest', sys.executable, str(dns_client), 'query', '192.168.9.1', 'proxy.test', transport, 'AAAA') == 'NOERROR'
            assert 'DNS UDP peer=10.88.0.2' in (results / 'fixtures.log').read_text()
            console.command('sha256sum -c /etc/old-config.sha')
            print('PASS: UDP/TCP split DNS, tunnel alternative and AAAA suppression', flush=True)
            console.command('ip -6 addr add 2001:db8:9::1/64 dev eth2 nodad; ip -6 addr add 2001:db8:2::2/64 dev eth1 nodad; ip -6 route add default via 2001:db8:2::1')
            ns('guest', 'ip', '-6', 'addr', 'add', '2001:db8:9::50/64', 'dev', 'nic', 'nodad')
            ns('guest', 'ip', '-6', 'route', 'add', 'default', 'via', '2001:db8:9::1')
            ns('inet', 'ip', '-6', 'route', 'add', '2001:db8:9::/64', 'via', '2001:db8:2::2')
            ns('guest', 'ping', '-6', '-c1', '-W3', '2001:db8:9::1')
            # Establish a working external IPv6 fixture before restoring refusal.
            console.command('nft delete table inet cpxy_rtp_guard; ip -6 rule add priority 11999 iif eth2 lookup main')
            assert fetch('[2001:db8:100::34]') == '2001:db8:9::50'
            console.command('/usr/libexec/cpxy-rtp/guard.sh; ip -6 rule del priority 11999 iif eth2 lookup main')
            try:
                fetch('[2001:db8:100::34]')
            except subprocess.CalledProcessError:
                pass
            else:
                raise AssertionError('external IPv6 escaped')
            try:
                ns('guest', sys.executable, '-c', "import socket;s=socket.socket(socket.AF_INET6,socket.SOCK_DGRAM);s.settimeout(2);s.connect(('2001:db8:9::1',53));s.send(bytes.fromhex('12340100000100000000000001610000010001'));s.recv(4096)")
            except subprocess.CalledProcessError as error:
                assert 'ConnectionRefusedError' in error.stderr, error.stderr
            else:
                raise AssertionError('IPv6 DNS bypassed the split resolver')
            print('PASS: working external IPv6 fixture, scoped refusal, local IPv6 and IPv6 DNS refusal', flush=True)
            console.command('/etc/init.d/cpxy-rtp-router reload; sleep 4; /etc/init.d/firewall reload', 150)
            wait_paths()
            console.command('kill -STOP $(pidof cpxy-rtp-router)')
            assert fetch('114.114.114.114') == '192.0.2.2'
            assert udp('114.114.114.114') == '192.0.2.2'
            try:
                fetch('93.184.216.34')
            except subprocess.CalledProcessError:
                pass
            else:
                raise AssertionError('traffic escaped suspended RTP')
            console.command('kill -CONT $(pidof cpxy-rtp-router)')
            wait_paths()
            ns('inet', 'ip', 'addr', 'add', '192.0.2.254/24', 'dev', 'nic')
            console.command('uci set network.wan.gateway=192.0.2.254; uci commit network; /etc/init.d/network reload; sleep 6', 120)
            wait_paths()
            print('PASS: reload, fw4 reload, suspended worker, WAN gateway change', flush=True)
            # Invalid/overlapping replacement must leave the running service intact.
            console.command("uci -q delete cpxy_rtp.main.network; uci add_list cpxy_rtp.main.network=lan; if /usr/libexec/cpxy-rtp/policy.sh prepare; then exit 1; fi; uci revert cpxy_rtp")
            paths()
            console.command('cp /etc/cpxy-rtp/state/main/client-id /tmp/client-id; kill -KILL $(pidof cpxy-rtp-router); sleep 8', 120)
            wait_paths()
            console.command('cmp /etc/cpxy-rtp/state/main/client-id /tmp/client-id')
            print('PASS: retained-router overlap rejected and crash recovery preserves identity/address', flush=True)
            console.send('reboot')
            console.until(rb'Please press Enter to activate this console', 150)
            console.send('')
            console.until(rb'root@[^\r\n]*#')
            console.command('sleep 25', 90)
            wait_paths()
            print('PASS: reboot restores policy and assigned address', flush=True)
            endpoint.terminate()
            endpoint.wait(timeout=10)
            console.command('/etc/init.d/cpxy-rtp-router restart; sleep 5', 120)
            assert fetch('114.114.114.114') == '192.0.2.2'
            for probe in [lambda: fetch('93.184.216.34'), lambda: udp('93.184.216.34')]:
                try:
                    probe()
                except subprocess.CalledProcessError:
                    pass
                else:
                    raise AssertionError('dead endpoint bypassed RTP')
            console.command('/etc/init.d/cpxy-rtp-router stop')
            try:
                fetch('93.184.216.34')
            except subprocess.CalledProcessError:
                pass
            else:
                raise AssertionError('stop removed guards')
            stop_probes.set()
            probe_thread.join(timeout=5)
            assert not probe_thread.is_alive()
            assert not (results / 'leak').exists(), (results / 'leak').read_text()
            (results / 'enforce').unlink()
            console.command('/etc/init.d/cpxy-rtp-router cleanup; test ! -e /etc/cpxy-rtp/devices; test -e /etc/config/cpxy', 120)
            assert fetch('93.184.216.34') == '192.0.2.2'
            console.command('opkg remove cpxy-rtp; test ! -e /usr/bin/cpxy-rtp-router; sha256sum -c /etc/old-config.sha', 120)
            print('PASS: endpoint loss/stop fail closed; cleanup/removal restores WAN and preserves old config', flush=True)
        except Exception:
            if 'console' in locals():
                with contextlib.suppress(Exception):
                    console.command('ip -4 rule show; ip -4 route show table 20000 default; ip -4 route show table 30000 default; ip addr show rtpr0; ubus call service list; logread | tail -40', 30)
            raise
        finally:
            stop_probes.set()
            if probe_thread:
                probe_thread.join(timeout=5)
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
            for process in processes:
                with contextlib.suppress(subprocess.TimeoutExpired):
                    process.wait(timeout=5)
                if process.poll() is None:
                    process.kill()
                    process.wait()


if __name__ == '__main__':
    if sys.argv[1:2] == ['--fixture']:
        fixture()
    else:
        main()
