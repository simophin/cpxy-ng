#!/usr/bin/env bash
# Optional root wrapper when the host has no /dev/net/tun:
# sudo unshare -nm bash rtp-service/tests/private-linux-lab.sh FIXTURE [ARGS...]
# Every mount/device/network mutation stays inside already-private namespaces.
set -euo pipefail
if [[ $(id -u) != 0 || $(readlink /proc/self/ns/net) == $(readlink /proc/1/ns/net) || $(readlink /proc/self/ns/mnt) == $(readlink /proc/1/ns/mnt) ]]; then
    echo 'Run with sudo unshare -nm; private network and mount namespaces required' >&2
    exit 1
fi
umask 077
mount --make-rprivate /
mount -t tmpfs -o mode=755 tmpfs /dev
mkdir /dev/net
mknod /dev/net/tun c 10 200
mknod -m 666 /dev/null c 1 3
mknod -m 666 /dev/zero c 1 5
mknod -m 666 /dev/random c 1 8
mknod -m 666 /dev/urandom c 1 9
ln -s /proc/self/fd /dev/fd
stage=$(mktemp -d /tmp/rtp-http-binaries-XXXX)
trap 'rm -rf "$stage"' EXIT
cp "${RTP_SERVICE_BIN:-target/debug/cpxy-rtp}" "$stage/cpxy-rtp"
export RTP_SERVICE_BIN="$stage/cpxy-rtp"
bash "$@"
