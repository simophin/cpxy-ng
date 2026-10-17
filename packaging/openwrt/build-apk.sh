#!/bin/sh
# Build a native OpenWrt apk v3 from the same binaries as build-ipk.sh.
# usage: build-apk.sh <apk arch> <version> <bin dir> <out dir>
# Requires apk-tools >= 3 with mkpkg, plus fakeroot when not running as root.
# APK_TOOL selects the apk executable; CPXY_APK_SIGN_KEY optionally signs it.
# No repository configuration or signing key is installed on the router.
set -eu

[ $# -eq 4 ] || { sed -n '2,6p' "$0" >&2; exit 2; }
if [ "$(id -u)" != 0 ]; then
	command -v fakeroot >/dev/null || { echo 'build-apk.sh needs fakeroot (or root) for root-owned files' >&2; exit 1; }
	exec fakeroot "$0" "$@"
fi
arch="$1" version="$2" bindir="$3" outdir="$4"
case "$arch" in aarch64 | x86_64) ;; *) echo "unsupported apk architecture: $arch" >&2; exit 2 ;; esac
apk="${APK_TOOL:-apk}"
"$apk" version --check "$version"
here="$(cd "$(dirname "$0")" && pwd)"
bindir="$(cd "$bindir" && pwd)"
mkdir -p "$outdir"
outdir="$(cd "$outdir" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/data/usr/bin" "$work/data/lib/apk/packages" "$work/scripts"
cp -R "$here/files/." "$work/data/"
cp "$here/../common/net.sh" "$work/data/usr/libexec/cpxy/net.sh"
for bin in cpxy-router cpxy-dns-split cpxy-client cpxy-tsgw; do
	install -m 0755 "$bindir/$bin" "$work/data/usr/bin/$bin"
done
chmod 0755 "$work/data/etc/init.d/"*
chmod 0644 "$work/data/lib/upgrade/keep.d/cpxy" "$work/data/usr/libexec/cpxy/"*
chmod 0600 "$work/data/etc/config/cpxy"
# OpenWrt's lifecycle/sysupgrade helpers consume these instead of opkg metadata.
cp "$here/control/conffiles" "$work/data/lib/apk/packages/cpxy.conffiles"
checksum="$(openssl dgst -sha256 "$work/data/etc/config/cpxy")"
printf '/etc/config/cpxy %s\n' "${checksum##* }" >"$work/data/lib/apk/packages/cpxy.conffiles_static"
(cd "$work/data" && find . -type f | sed 's|^\./|/|' | sort) >"$work/data/lib/apk/packages/cpxy.list"
chmod 0644 "$work/data/lib/apk/packages/"*

# Upgrade hooks reuse the install/remove hooks with OpenWrt's upgrade marker.
for action in install remove; do
	{
		echo '#!/bin/sh'
		echo 'export PKG_UPGRADE=1'
		sed '1d' "$here/apk/$action"
	} >"$work/scripts/upgrade-$action"
done
set -- "$apk" mkpkg --compat 3.0.0 --no-xattrs \
	--info name:cpxy --info "version:$version" --info "arch:$arch" \
	--info 'description:cpxy proxy, split DNS, LAN routing and Tailscale exit node' \
	--info origin:cpxy-ng --info maintainer:cpxy-ng \
	--info url:https://github.com/simophin/cpxy-ng \
	--info 'depends:ip-full kmod-tun !cpxy-router' \
	--script "post-install:$here/apk/install" \
	--script "pre-upgrade:$work/scripts/upgrade-remove" \
	--script "post-upgrade:$work/scripts/upgrade-install" \
	--script "pre-deinstall:$here/apk/remove" \
	--script "post-deinstall:$here/apk/post-remove" \
	--files "$work/data" --output "$outdir/cpxy-$version.apk"
[ -z "${CPXY_APK_SIGN_KEY:-}" ] || set -- "$@" --sign "$CPXY_APK_SIGN_KEY"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}" "$@"
echo "$outdir/cpxy-$version.apk"
