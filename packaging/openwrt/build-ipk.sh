#!/bin/sh
# Assemble cpxy-router_<version>_<arch>.ipk from prebuilt binaries. No OpenWrt SDK needed.
#
# usage: build-ipk.sh <opkg arch> <version> <bin dir> <out dir>
#   <opkg arch>  what `opkg print-architecture` shows on the router, e.g. aarch64_cortex-a53
#   <bin dir>    holds cpxy-client, cpxy-dns-split and cpxy-tun2proxy for that architecture
set -eu

[ $# -eq 4 ] || { sed -n '2,6p' "$0" >&2; exit 2; }
arch="$1" version="$2" bindir="$3" outdir="$4"

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/data/usr/bin" "$work/control" "$outdir"
# The archives are written from inside other directories, so use absolute paths
bindir="$(cd "$bindir" && pwd)"
outdir="$(cd "$outdir" && pwd)"
cp -R "$here/files/." "$work/data/"
for bin in cpxy-client cpxy-dns-split cpxy-tun2proxy; do
	install -m 0755 "$bindir/$bin" "$work/data/usr/bin/$bin"
done
chmod 0755 "$work/data/etc/init.d/cpxy" "$work/data/etc/uci-defaults/90-cpxy"
chmod 0644 "$work/data/etc/config/cpxy" "$work/data/usr/libexec/cpxy/net.sh"

sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" "$here/control/control.in" >"$work/control/control"
cp "$here/control/conffiles" "$here/control/postinst" "$here/control/prerm" "$here/control/postrm" "$work/control/"
chmod 0755 "$work/control/postinst" "$work/control/prerm" "$work/control/postrm"

# Reproducible, root-owned archives
tar_opts="--owner=0 --group=0 --numeric-owner --sort=name --mtime=@0 --format=gnu"
(cd "$work/data" && tar $tar_opts -czf "$work/data.tar.gz" .)
(cd "$work/control" && tar $tar_opts -czf "$work/control.tar.gz" .)
echo "2.0" >"$work/debian-binary"

ipk="$outdir/cpxy-router_${version}_${arch}.ipk"
(cd "$work" && tar $tar_opts -czf "$ipk.tmp" ./debian-binary ./control.tar.gz ./data.tar.gz)
mv "$ipk.tmp" "$ipk"
echo "$ipk"
