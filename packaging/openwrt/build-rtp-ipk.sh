#!/bin/sh
# build-rtp-ipk.sh ARCH VERSION BIN_DIR OUT_DIR
set -eu
[ "$#" = 4 ] || exit 2
arch="$1" version="$2" bindir="$(cd "$3" && pwd)"
mkdir -p "$4"; out="$(cd "$4" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
mkdir -p "$work/data/usr/bin" "$work/control"
cp -R "$here/rtp-files/." "$work/data/"
for bin in cpxy-rtp cpxy-rtp-router; do install -m755 "$bindir/$bin" "$work/data/usr/bin/$bin"; done
chmod 600 "$work/data/etc/config/cpxy_rtp"
cat >"$work/control/control" <<EOT
Package: cpxy-rtp
Version: $version
Architecture: $arch
Maintainer: CPXY contributors
Section: net
Priority: optional
Depends: libc, ip-full, kmod-tun, firewall4
Description: HTTP-controlled RTP VPN and opt-in regional OpenWrt router
EOT
printf '/etc/config/cpxy_rtp\n' >"$work/control/conffiles"
cat >"$work/control/postinst" <<'EOT'
#!/bin/sh
[ -z "$IPKG_INSTROOT" ] || exit 0
/etc/init.d/cpxy-rtp-router enable
/etc/init.d/cpxy-rtp-router restart
exit 0
EOT
cat >"$work/control/prerm" <<'EOT'
#!/bin/sh
[ -z "$IPKG_INSTROOT" ] || exit 0
if [ "$PKG_UPGRADE" = 1 ]; then
 /etc/init.d/cpxy-rtp-router stop
else
 /etc/init.d/cpxy-rtp-router cleanup
 /etc/init.d/cpxy-rtp-router disable
fi
exit 0
EOT
chmod 755 "$work/control/postinst" "$work/control/prerm"
opts='--owner=0 --group=0 --numeric-owner --sort=name --mtime=@0 --format=gnu'
(cd "$work/data" && tar $opts -czf "$work/data.tar.gz" .)
(cd "$work/control" && tar $opts -czf "$work/control.tar.gz" .)
printf '2.0\n' >"$work/debian-binary"
file="$out/cpxy-rtp_${version}_${arch}.ipk"
(cd "$work" && tar $opts -czf "$file" ./debian-binary ./control.tar.gz ./data.tar.gz)
printf '%s\n' "$file"
