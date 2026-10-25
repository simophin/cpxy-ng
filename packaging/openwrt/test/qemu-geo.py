#!/usr/bin/env python3
"""Booted OpenWrt integration, isolated with unshare -Urmn.

Usage: unshare -Urmn python3 qemu-geo.py IMAGE IPK DEPS_DIR SERVER RESULTS_DIR
IMAGE is an official x86-64 combined ext4 image; DEPS_DIR contains matching dependency ipks.
Requires iproute2, QEMU, curl and dig. No third-party Python packages or host bridges needed.
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
                if len(frame) < 54 or frame[12:14] != b'\x08\x00':
                    continue
                ip = frame[14:]
                offset = (ip[0] & 15) * 4
                if (ip[9] == 6 and ip[12:16] == socket.inet_aton('192.0.2.2')
                        and ip[16:20] == socket.inet_aton('93.184.216.34')
                        and struct.unpack('!H', ip[offset+2:offset+4])[0] == 8000
                        and (results / 'enforce').exists()):
                    (results / 'leak').write_text('Non-regional TCP escaped directly to WAN')

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
            sock.sendto(answer(query), self.client_address)

    class TCP(socketserver.BaseRequestHandler):
        def handle(self):
            print(f'DNS TCP peer={self.client_address[0]}', flush=True)
            f = self.request.makefile('rb')
            while header := f.read(2):
                data = answer(f.read(struct.unpack('!H', header)[0]))
                self.request.sendall(struct.pack('!H', len(data)) + data)

    class V6Web(http.server.ThreadingHTTPServer):
        address_family = socket.AF_INET6

    for server in [socketserver.ThreadingUDPServer(('192.0.2.1', 53), UDP),
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
        self.sequence += 1
        marker = f'__CPXY_{self.sequence}'
        self.send(command + f"; printf '\\n{marker}:%s\\n' \"$?\"")
        output, match = self.until(marker.encode() + rb':([0-9]+)\r?\n', timeout)
        assert match[1] == b'0', f'Guest command failed: {command}\n{output}'
        return output


def main():
    image, package, deps, server, results = map(lambda p: Path(p).resolve(), sys.argv[1:])
    if os.getuid() != 0 or Path('/proc/self/uid_map').read_text().split()[-1] != '1':
        raise SystemExit('Run in a disposable namespace with unshare -Urmn, as documented.')
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
    for addr in ['93.184.216.34/32', '114.114.114.114/32']:
        ns('inet', 'ip', 'addr', 'add', addr, 'dev', 'lo')
    run('ip', 'link', 'add', 'uppeer', 'type', 'veth', 'peer', 'name', 'upnic')
    run('ip', 'link', 'set', 'upnic', 'netns', 'upstream')
    run('ip', 'link', 'set', 'uppeer', 'master', 'br1')
    run('ip', 'link', 'set', 'uppeer', 'up')
    ns('upstream', 'ip', 'addr', 'add', '192.0.2.3/24', 'dev', 'upnic')
    ns('upstream', 'ip', 'link', 'set', 'upnic', 'up')
    ns('upstream', 'ip', 'route', 'add', 'default', 'via', '192.0.2.1')
    ns('inet', 'sysctl', '-qw', 'net.ipv4.ip_forward=1')
    ns('inet', 'ip', '-6', 'addr', 'add', '2001:db8:100::34/128', 'dev', 'lo', 'nodad')
    ns('inet', 'ip', '-6', 'addr', 'add', '2001:db8:2::1/64', 'dev', 'nic', 'nodad')
    ns('lan', 'ip', '-6', 'addr', 'add', '2001:db8:8::50/64', 'dev', 'nic', 'nodad')
    ns('lan', 'ip', '-6', 'route', 'add', 'default', 'via', '2001:db8:8::1')
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
            background(['ip', 'netns', 'exec', 'inet', str(server), '--key', 'vm-test', '192.0.2.1:8443'], 'server.log')
            background(['ip', 'netns', 'exec', 'upstream', str(server), '--key', 'vm-guest', '192.0.2.3:8443'], 'guest-server.log')
            # Serve only test assets; never expose the repository or credentials.
            assets = results / 'assets'
            assets.mkdir(exist_ok=True)
            import shutil
            shutil.copy(package, assets / 'cpxy.ipk')
            for dep in deps.glob('*.ipk'):
                shutil.copy(dep, assets / dep.name)
            background(['ip', 'netns', 'exec', 'inet', sys.executable, '-m', 'http.server', '8080', '--directory', str(assets)], 'assets.log')
            args = ['qemu-system-x86_64', '-m', '256', '-smp', '2', '-display', 'none',
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
            console.command('sleep 25')
            console.command("uci set network.lan.ipaddr=192.168.8.1; uci set network.wan.proto=static; uci set network.wan.ipaddr=192.0.2.2; uci set network.wan.netmask=255.255.255.0; uci set network.wan.gateway=192.0.2.1; uci commit network; /etc/init.d/network restart")
            console.command('sleep 3; ping -c1 -W3 192.0.2.1')
            for name in [p.name for p in assets.glob('*.ipk')]:
                console.command(f'wget -q http://192.0.2.1:8080/{shlex.quote(name)} -O /tmp/{shlex.quote(name)}')
            console.command('opkg install /tmp/*.ipk', 120)
            console.command("uci set network.guest=interface; uci set network.guest.device=eth2; uci set network.guest.proto=static; uci set network.guest.ipaddr=192.168.9.1; uci set network.guest.netmask=255.255.255.0; uci commit network; /etc/init.d/network reload; sleep 3")
            console.command("uci set firewall.guests=zone; uci set firewall.guests.name=guests; uci add_list firewall.guests.network=guest; uci set firewall.guests.input=ACCEPT; uci set firewall.guests.output=ACCEPT; uci set firewall.guests.forward=REJECT; uci set firewall.guestwan=forwarding; uci set firewall.guestwan.src=guests; uci set firewall.guestwan.dest=wan; uci commit firewall; /etc/init.d/firewall reload")
            console.command("uci set cpxy.guest=instance; uci set cpxy.guest.index=1; uci add_list cpxy.guest.network=guest; uci set cpxy.guest.server='http://:vm-guest@192.0.2.3:8443'")
            console.command("uci set cpxy.defaults.server='http://:vm-test@192.0.2.1:8443'; uci set cpxy.defaults.client=0; uci set cpxy.defaults.tsgw=0; uci set cpxy.defaults.router_geoip=1; uci -q delete cpxy.defaults.dns_upstream; uci add_list cpxy.defaults.dns_upstream=192.0.2.1; uci add_list cpxy.defaults.dns_alternative='tcp://192.0.2.1?via=cpxy'; uci commit cpxy; /etc/init.d/cpxy-router enable; /etc/init.d/cpxy-router start", 120)

            def fetch(ip, source='lan'):
                return ns(source, 'curl', '-fsS', '--noproxy', '*', '--max-time', '8', f'http://{ip}:8000/')

            def paths():
                regional = fetch('114.114.114.114')
                proxied = fetch('93.184.216.34')
                assert regional == '192.0.2.2', f'regional path: {regional}'
                assert proxied == '93.184.216.34', f'proxy path: {proxied}'

            console.command('sleep 3; ip -4 rule show; ip -4 route show table 100 | wc -l')
            (results / 'enforce').touch()
            def probe():
                while not stop_probes.is_set():
                    try:
                        source = ns('lan', 'curl', '-fsS', '--noproxy', '*', '--max-time', '1', 'http://93.184.216.34:8000/')
                        if source != '93.184.216.34':
                            (results / 'leak').write_text(f'Continuous TCP probe escaped via {source}')
                    except subprocess.CalledProcessError:
                        pass  # Refusal/unavailability is permitted during a transition.
                    stop_probes.wait(0.2)
            probe_thread = threading.Thread(target=probe, daemon=True)
            probe_thread.start()
            paths()
            assert fetch('93.184.216.34', 'guest') == '192.0.2.3'
            assert fetch('114.114.114.114', 'guest') == '192.0.2.2'
            print('PASS: VM regional and proxy paths', flush=True)
            assert ns('lan', 'dig', '+short', '+time=3', '+tries=1', '@192.168.8.1', 'regional.test', 'A') == '114.114.114.114'
            assert ns('lan', 'dig', '+short', '+time=3', '+tries=1', '@192.168.8.1', 'proxy.test', 'A') == '93.184.216.34'
            assert not ns('lan', 'dig', '+short', '+time=3', '+tries=1', '@192.168.8.1', 'proxy.test', 'AAAA')
            assert ns('lan', 'dig', '+tcp', '+short', '+time=3', '+tries=1', '@192.168.8.1', 'proxy.test', 'A') == '93.184.216.34'
            assert 'DNS TCP peer=192.0.2.1' in (results / 'fixtures.log').read_text()
            print('PASS: VM split DNS, proxied alternative, IPv4-only answers', flush=True)
            console.command('ip -6 addr add 2001:db8:8::1/64 dev br-lan nodad; ip -6 addr add 2001:db8:2::2/64 dev eth1 nodad; ip -6 route add default via 2001:db8:2::1')
            ns('lan', 'ping', '-6', '-c1', '-W3', '2001:db8:8::1')
            try:
                fetch('[2001:db8:100::34]')
            except subprocess.CalledProcessError:
                pass
            else:
                raise AssertionError('IPv6 bypassed the IPv4 policy')
            print('PASS: VM local IPv6 remains reachable; internet IPv6 is refused', flush=True)
            console.command("before=$(pidof cpxy-router); /etc/init.d/cpxy-router reload; sleep 2; test \"$before\" = \"$(pidof cpxy-router)\"")
            paths()
            console.command('/etc/init.d/firewall reload; sleep 2')
            paths()
            print('PASS: VM unchanged reload and actual firewall reload', flush=True)
            ns('inet', 'ip', 'addr', 'add', '192.0.2.254/24', 'dev', 'nic')
            console.command('uci set network.wan.gateway=192.0.2.254; uci commit network; /etc/init.d/network reload; sleep 5', 120)
            # netifd's interface event is asynchronous, especially under software emulation.
            console.command("for n in $(seq 1 60); do active=$(ip -4 rule show priority 9103 | awk '/lookup/ {print $NF; exit}'); ip -4 route show table \"$active\" | grep -q 'via 192.0.2.254' && break; sleep 1; done; ip -4 route show table \"$active\" | grep -q 'via 192.0.2.254'", 150)
            paths()
            console.command("ip -4 route show table 100 | grep -q 'via 192.0.2.254' || ip -4 route show table 10100 | grep -q 'via 192.0.2.254'")
            print('PASS: VM netifd gateway change reconciles direct routes', flush=True)
            console.command('kill -STOP $(pidof cpxy-router)')
            assert fetch('114.114.114.114') == '192.0.2.2'
            try:
                fetch('93.184.216.34')
            except subprocess.CalledProcessError:
                pass
            else:
                raise AssertionError('proxy escaped while worker suspended')
            console.command('kill -CONT $(pidof cpxy-router)')
            paths()
            console.command("uci add_list cpxy.main.proxy_route=114.0.0.0/8; uci commit cpxy; /etc/init.d/cpxy-router reload; sleep 2", 120)
            assert fetch('114.114.114.114') == '114.114.114.114'
            print('PASS: VM worker failure and broad proxy override', flush=True)
            console.command("before=$(pidof cpxy-router); cp /usr/bin/cpxy-router /tmp/router-next; printf '\\n' >>/tmp/router-next; chmod 755 /tmp/router-next; mv /tmp/router-next /usr/bin/cpxy-router; /etc/init.d/cpxy-router reload; sleep 3; test \"$before\" != \"$(pidof cpxy-router)\"", 120)
            assert fetch('93.184.216.34') == '93.184.216.34'
            print('PASS: VM binary/data replacement restarts DNS and packet workers', flush=True)
            console.send('reboot')
            console.until(rb'Please press Enter to activate this console', 150)
            console.send('')
            console.until(rb'root@[^\r\n]*#')
            console.command("for n in $(seq 1 90); do test \"$(pidof cpxy-router | wc -w)\" -eq 2 && netstat -lnt | grep -q ':5335 ' && netstat -lnt | grep -q ':5345 ' && break; sleep 1; done; netstat -lnt | grep -q ':5335 '; netstat -lnt | grep -q ':5345 '", 150)
            assert fetch('114.114.114.114') == '114.114.114.114'
            assert fetch('93.184.216.34') == '93.184.216.34'
            assert fetch('93.184.216.34', 'guest') == '192.0.2.3'
            print('PASS: VM persistent config and service restart after reboot', flush=True)
            stop_probes.set()
            probe_thread.join(timeout=3)
            assert not (results / 'leak').exists(), (results / 'leak').read_text()
            print('PASS: VM continuous probes and WAN capture show no TCP bypass through reboot/reloads', flush=True)
            (results / 'enforce').unlink()
            console.command('/etc/init.d/cpxy-router stop; opkg remove cpxy; ip -4 rule show; uci show firewall', 120)
            assert fetch('93.184.216.34') == '192.0.2.2'
            print('PASS: VM stop/removal restores WAN', flush=True)
        except Exception:
            if 'console' in locals():
                with contextlib.suppress(Exception):
                    console.command('ip -4 rule show; ip -4 route show table main; logread -e cpxy | tail -40', 30)
            raise
        finally:
            stop_probes.set()
            if probe_thread:
                probe_thread.join(timeout=3)
            for process in reversed(processes):
                process.terminate()
            for process in processes:
                with contextlib.suppress(subprocess.TimeoutExpired):
                    process.wait(timeout=5)
                if process.poll() is None:
                    process.kill()


if __name__ == '__main__':
    if sys.argv[1:2] == ['--fixture']:
        fixture()
    else:
        main()
