#!/bin/sh
# Assemble cpxy-gateway_<version>_<arch>.deb from prebuilt static binaries. Needs only dpkg-deb.
#
# usage: build-deb.sh <deb arch> <version> <bin dir> <out dir>
#   <deb arch>  amd64 or arm64
#   <version>   a Debian version, e.g. 1.2.3 or 0.0.0~git.abc1234
#   <bin dir>   holds cpxy-router and cpxy-dns-split for that architecture
set -eu

[ $# -eq 4 ] || { sed -n '2,7p' "$0" >&2; exit 2; }
arch="$1" version="$2" bindir="$3" outdir="$4"

here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
root="$work/root"

mkdir -p "$root/DEBIAN" "$root/usr/bin" "$outdir"
cp -R "$here/files/." "$root/"
cp "$here/../common/net.sh" "$root/usr/libexec/cpxy/net.sh"
for bin in cpxy-router cpxy-dns-split; do
	install -m 0755 "$bindir/$bin" "$root/usr/bin/$bin"
done
chmod 0755 "$root/usr/libexec/cpxy/gateway.sh" "$root/usr/libexec/cpxy/dns.sh"
chmod 0644 "$root/usr/libexec/cpxy/config.sh" "$root/usr/libexec/cpxy/net.sh" \
	"$root/usr/lib/systemd/system/"*.service "$root/usr/lib/sysctl.d/60-cpxy.conf"
# Holds the server's key
chmod 0600 "$root/etc/cpxy/cpxy.conf"

sed -e "s/@VERSION@/$version/" -e "s/@ARCH@/$arch/" "$here/control/control.in" >"$root/DEBIAN/control"
cp "$here/control/conffiles" "$here/control/postinst" "$here/control/prerm" "$here/control/postrm" "$root/DEBIAN/"
chmod 0644 "$root/DEBIAN/control" "$root/DEBIAN/conffiles"
chmod 0755 "$root/DEBIAN/postinst" "$root/DEBIAN/prerm" "$root/DEBIAN/postrm"
find "$root" -type d -exec chmod 0755 {} +

deb="$outdir/cpxy-gateway_${version}_${arch}.deb"
# xz: readable by every dpkg still in use (Debian 11's cannot read zstd)
SOURCE_DATE_EPOCH=0 dpkg-deb --root-owner-group -Zxz --build "$root" "$deb" >/dev/null
echo "$deb"
