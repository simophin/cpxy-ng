#!/bin/sh
# build-rtp-apk.sh APK_ARCH VERSION BIN_DIR OUT_DIR (apk-tools >=3)
set -eu
[ "$#" = 4 ] || exit 2
if [ "$(id -u)" != 0 ]; then exec fakeroot "$0" "$@"; fi
arch="$1" version="$2" bindir="$(cd "$3" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
"$here/apk-architectures.sh" --all | tr ' ' '\n' | grep -Fqx "$arch"
apk="${APK_TOOL:-apk}"
"$apk" version --check "$version"
mkdir -p "$4"; out="$(cd "$4" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
mkdir -p "$work/data/usr/bin" "$work/data/lib/apk/packages"
cp -R "$here/rtp-files/." "$work/data/"
for bin in cpxy-rtp cpxy-rtp-router; do install -m755 "$bindir/$bin" "$work/data/usr/bin/$bin"; done
chmod 600 "$work/data/etc/config/cpxy_rtp"
printf '/etc/config/cpxy_rtp\n' >"$work/data/lib/apk/packages/cpxy-rtp.conffiles"
checksum="$(openssl dgst -sha256 "$work/data/etc/config/cpxy_rtp")"
printf '/etc/config/cpxy_rtp %s\n' "${checksum##* }" >"$work/data/lib/apk/packages/cpxy-rtp.conffiles_static"
(cd "$work/data" && find . -type f | sed 's|^\./|/|' | sort) >"$work/data/lib/apk/packages/cpxy-rtp.list"
cat >"$work/install" <<'EOT'
#!/bin/sh
/etc/init.d/cpxy-rtp-router enable
/etc/init.d/cpxy-rtp-router restart
EOT
cat >"$work/remove" <<'EOT'
#!/bin/sh
/etc/init.d/cpxy-rtp-router cleanup
/etc/init.d/cpxy-rtp-router disable
EOT
printf '#!/bin/sh\n/etc/init.d/cpxy-rtp-router stop\n' >"$work/upgrade"
chmod 755 "$work/install" "$work/remove" "$work/upgrade"
set -- "$apk" mkpkg --compat 3.0.0 --no-xattrs --info name:cpxy-rtp \
 --info "version:$version" --info "arch:$arch" --info origin:cpxy-ng \
 --info maintainer:cpxy-ng --info url:https://github.com/simophin/cpxy-ng \
 --info 'description:HTTP-controlled RTP VPN and regional OpenWrt router' \
 --info 'depends:ip-full kmod-tun firewall4' --files "$work/data" \
 --script "post-install:$work/install" --script "pre-deinstall:$work/remove" \
 --script "pre-upgrade:$work/upgrade" --script "post-upgrade:$work/install" \
 --output "$out/cpxy-rtp-$version.apk"
[ -z "${CPXY_APK_SIGN_KEY:-}" ] || set -- "$@" --sign "$CPXY_APK_SIGN_KEY"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}" "$@"
