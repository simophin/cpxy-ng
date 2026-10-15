#!/bin/sh
# Turn a directory holding one or more ipks into an opkg feed: Packages and Packages.gz, plus
# Packages.sig when a usign secret key is given.
#
# usage: make-feed.sh <feed dir> [usign secret key file]
set -eu

[ $# -ge 1 ] && [ $# -le 2 ] || { sed -n '2,5p' "$0" >&2; exit 2; }
feed="$(cd "$1" && pwd)" key="${2:-}"
[ -z "$key" ] || key="$(cd "$(dirname "$key")" && pwd)/$(basename "$key")"
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

: >"$feed/Packages"
for ipk in "$feed"/*.ipk; do
	[ -e "$ipk" ] || { echo "no ipk in $feed" >&2; exit 1; }
	rm -rf "$work/x" && mkdir "$work/x"
	(cd "$work/x" && tar -xzf "$ipk" ./control.tar.gz && tar -xzf control.tar.gz ./control)
	# The control fields, then where the file is and how to check it
	sed -e '/^[[:space:]]*$/d' "$work/x/control" >>"$feed/Packages"
	{
		echo "Filename: $(basename "$ipk")"
		echo "Size: $(wc -c <"$ipk" | tr -d ' ')"
		echo "SHA256sum: $(sha256sum "$ipk" | cut -d' ' -f1)"
		echo
	} >>"$feed/Packages"
done
gzip -9nc "$feed/Packages" >"$feed/Packages.gz"
[ -z "$key" ] || node "$here/usign-sign.mjs" "$feed/Packages" "$key"
ls -l "$feed"
