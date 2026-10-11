#!/bin/sh
# Assemble cpxy-tsgw_<version>_<arch>.deb from a prebuilt static cpxy-tsgw. Needs only dpkg-deb.
#
# usage: build-deb.sh <deb arch> <version> <binary> <out dir>
#   <deb arch>  amd64 or arm64
#   <version>   a Debian version, e.g. 1.2.3 or 0.0.0~git.abc1234
#   <binary>    cpxy-tsgw for that architecture (gateway/build.sh <arch>-unknown-linux-musl)
set -eu

[ $# -eq 4 ] || { sed -n '2,7p' "$0" >&2; exit 2; }
arch="$1" version="$2" binary="$3" outdir="$4"

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
root="$work/root"

mkdir -p "$root/DEBIAN" "$root/usr/bin" "$outdir"
cp -R "$here/files/." "$root/"
install -m 0755 "$binary" "$root/usr/bin/cpxy-tsgw"
chmod 0644 "$root/usr/lib/systemd/system/cpxy-tsgw.service"
# Holds the server's key, and maybe a Tailscale auth key
chmod 0600 "$root/etc/cpxy/cpxy-tsgw.conf"

sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" "$here/control/control.in" >"$root/DEBIAN/control"
cp "$here/control/conffiles" "$here/control/postinst" "$here/control/prerm" "$here/control/postrm" "$root/DEBIAN/"
chmod 0644 "$root/DEBIAN/control" "$root/DEBIAN/conffiles"
chmod 0755 "$root/DEBIAN/postinst" "$root/DEBIAN/prerm" "$root/DEBIAN/postrm"
find "$root" -type d -exec chmod 0755 {} +

deb="$outdir/cpxy-tsgw_${version}_${arch}.deb"
# xz: readable by every dpkg still in use (Debian 11's cannot read zstd)
SOURCE_DATE_EPOCH=0 dpkg-deb --root-owner-group -Zxz --build "$root" "$deb" >/dev/null
echo "$deb"
