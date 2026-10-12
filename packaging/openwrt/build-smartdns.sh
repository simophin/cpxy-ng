#!/bin/sh
# Build a static smartdns for the cpxy package, from a pinned upstream release. Runs in Alpine
# (musl), natively on the target architecture, e.g.:
#   docker run --rm -v "$PWD:/w" -w /w alpine:3.22 packaging/openwrt/build-smartdns.sh bins
#
# usage: build-smartdns.sh <out dir>
#   writes cpxy-smartdns, smartdns-LICENSE and smartdns-SOURCE into <out dir>
set -eu

[ $# -eq 1 ] || { sed -n '2,8p' "$0" >&2; exit 2; }
out="$1"

repo=https://github.com/pymumu/smartdns
tag=Release48.4
commit=21c940edc65520849ba03544c5cf8d9cf326e680

# Static OpenSSL and zlib link into the binary, so it needs nothing on the router
apk add --no-cache build-base git linux-headers openssl-dev openssl-libs-static zlib-dev zlib-static >/dev/null

src="$(mktemp -d)"
trap 'rm -rf "$src"' EXIT
git -c advice.detachedHead=false clone -q --depth 1 --branch "$tag" "$repo" "$src"
[ "$(git -C "$src" rev-parse HEAD)" = "$commit" ] || { echo "$tag is not $commit" >&2; exit 1; }

make -C "$src/src" -j"$(nproc)" STATIC=yes VER="$tag" >/dev/null
mkdir -p "$out"
install -m 0755 "$src/src/smartdns" "$out/cpxy-smartdns"
strip "$out/cpxy-smartdns"
cp "$src/LICENSE" "$out/smartdns-LICENSE"
printf 'smartdns %s (%s), built unmodified from %s with STATIC=yes\n' "$tag" "$commit" "$repo" >"$out/smartdns-SOURCE"
"$out/cpxy-smartdns" -v
