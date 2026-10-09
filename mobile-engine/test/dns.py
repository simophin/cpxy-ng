"""Minimal DNS server and client for tun-lab.sh, using only the standard library.

  dns.py serve <ip> <delay ms> <name>=<ipv4>...   answer A queries for these names, NXDOMAIN otherwise
  dns.py query <server> <name> [udp|tcp] [A|AAAA] print the rcode and the A records of the answer
"""
import socket
import struct
import sys
import threading
import time

TYPES = {"A": 1, "AAAA": 28}
RCODES = {0: "NOERROR", 2: "SERVFAIL", 3: "NXDOMAIN"}


def parse_question(msg):
    """Returns (name, qtype, end offset) of the first question."""
    pos, labels = 12, []
    while msg[pos]:
        labels.append(msg[pos + 1 : pos + 1 + msg[pos]].decode())
        pos += 1 + msg[pos]
    qtype, _ = struct.unpack("!HH", msg[pos + 1 : pos + 5])
    return ".".join(labels).lower(), qtype, pos + 5


def answer(query, records):
    name, qtype, end = parse_question(query)
    ips = records.get(name)
    rcode = 3 if ips is None else 0
    answers = b""
    if ips and qtype == TYPES["A"]:
        for ip in ips:
            answers += struct.pack("!HHHIH", 0xC00C, 1, 1, 60, 4) + socket.inet_aton(ip)
    count = len(answers) // 16
    header = struct.pack("!HHHHHH", struct.unpack("!H", query[:2])[0], 0x8180 | rcode, 1, count, 0, 0)
    return header + query[12:end] + answers


def serve(ip, delay_ms, *specs):
    records = {}
    for spec in specs:
        name, addr = spec.split("=")
        records.setdefault(name.lower(), []).append(addr)
    delay = int(delay_ms) / 1000

    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind((ip, 53))

    def reply_udp(data, peer):
        time.sleep(delay)
        udp.sendto(answer(data, records), peer)

    tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    tcp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    tcp.bind((ip, 53))
    tcp.listen()

    def serve_tcp():
        while True:
            conn, _ = tcp.accept()
            with conn:
                length = struct.unpack("!H", recv_exact(conn, 2))[0]
                reply = answer(recv_exact(conn, length), records)
                time.sleep(delay)
                conn.sendall(struct.pack("!H", len(reply)) + reply)

    threading.Thread(target=serve_tcp, daemon=True).start()
    while True:
        data, peer = udp.recvfrom(4096)
        threading.Thread(target=reply_udp, args=(data, peer), daemon=True).start()


def recv_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def query(server, name, transport="udp", qtype="A"):
    qname = b"".join(bytes([len(label)]) + label.encode() for label in name.split(".")) + b"\0"
    msg = struct.pack("!HHHHHH", 0x1234, 0x0100, 1, 0, 0, 0) + qname + struct.pack("!HH", TYPES[qtype], 1)
    if transport == "udp":
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout(3)
        s.sendto(msg, (server, 53))
        reply = s.recv(4096)
    else:
        s = socket.create_connection((server, 53), timeout=3)
        s.sendall(struct.pack("!H", len(msg)) + msg)
        reply = recv_exact(s, struct.unpack("!H", recv_exact(s, 2))[0])

    rcode = struct.unpack("!H", reply[2:4])[0] & 0xF
    count = struct.unpack("!H", reply[6:8])[0]
    _, _, pos = parse_question(reply)
    ips = []
    for _ in range(count):
        # Names in answers are compression pointers here (both servers write them)
        pos += 2 if reply[pos] & 0xC0 else reply.index(0, pos) - pos + 1
        rtype, _, _, rdlen = struct.unpack("!HHIH", reply[pos : pos + 10])
        pos += 10
        if rtype == 1:
            ips.append(socket.inet_ntoa(reply[pos : pos + rdlen]))
        pos += rdlen
    print(RCODES.get(rcode, rcode), *ips)


if __name__ == "__main__":
    {"serve": serve, "query": query}[sys.argv[1]](*sys.argv[2:])
