#!/usr/bin/env python3
"""Fetch a pinned disposable OpenWrt QEMU image and its ip-full/TUN dependencies.

Usage: fetch-rtp-qemu-assets.py DIRECTORY
All artifacts come from official release feeds and are checked against SHA-256.
The firmware hash is pinned to the same release as the existing QEMU fixture.
"""
import gzip
import hashlib
import json
from pathlib import Path
import re
import sys
import urllib.request
import zlib

BASE = 'https://downloads.openwrt.org/releases/24.10.0'
IMAGE = 'openwrt-24.10.0-x86-64-generic-ext4-combined.img.gz'
IMAGE_SHA = 'a873d9f2e6e667d03dd06f75ed1f1b580beb7ce4337d5a44b60911a266965fb5'
FEEDS = [BASE + '/targets/x86/64/packages', BASE + '/packages/x86_64/base',
         BASE + '/targets/x86/64/kmods/6.6.73-1-a21259e4f338051d27a6443a3a7f7f1f']


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        body = response.read(64 * 1024 * 1024 + 1)
    if len(body) > 64 * 1024 * 1024:
        raise ValueError('Unexpectedly large fixture asset')
    return body


def main():
    root = Path(sys.argv[1]).resolve()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    deps = root / 'deps'
    deps.mkdir(exist_ok=True)
    image = root / IMAGE
    body = image.read_bytes() if image.exists() else fetch(BASE + '/targets/x86/64/' + IMAGE)
    assert hashlib.sha256(body).hexdigest() == IMAGE_SHA, 'Firmware checksum mismatch'
    image.write_bytes(body)
    # OpenWrt appends fwtool signature metadata after the gzip member. The
    # pinned whole-file hash authenticates it; inflate only the image member.
    inflater = zlib.decompressobj(16 + zlib.MAX_WBITS)
    raw = inflater.decompress(body, 512 * 1024 * 1024 + 1)
    assert inflater.eof and len(raw) <= 512 * 1024 * 1024, 'Invalid image size'
    (root / 'openwrt.img').write_bytes(raw)
    packages = {}
    for feed in FEEDS:
        text = gzip.decompress(fetch(feed + '/Packages.gz')).decode()
        for paragraph in text.split('\n\n'):
            fields = dict(line.split(': ', 1) for line in paragraph.splitlines() if ': ' in line and not line.startswith(' '))
            if 'Package' in fields:
                packages[fields['Package']] = (feed, fields)
    pending = ['ip-full', 'kmod-tun']
    seen = {'libc', 'libgcc', 'libpthread', 'librt', 'kernel'}
    manifest = {'image': IMAGE, 'image_sha256': IMAGE_SHA, 'packages': {}}
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        feed, fields = packages[name]
        filename = fields['Filename']
        body = fetch(feed + '/' + filename)
        digest = hashlib.sha256(body).hexdigest()
        assert digest == fields['SHA256sum'], 'Package checksum mismatch: ' + name
        (deps / Path(filename).name).write_bytes(body)
        manifest['packages'][name] = {'version': fields['Version'], 'sha256': digest}
        for dependency in fields.get('Depends', '').split(','):
            match = re.match(r'\s*([A-Za-z0-9+_.-]+)', dependency)
            if match:
                pending.append(match[1])
    (root / 'firmware-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print('Pinned OpenWrt firmware and dependency hashes verified')


if __name__ == '__main__':
    main()
