#!/bin/sh
# Package one set of generic static binaries for every supported apk label.
# usage: build-apk-variants.sh <rust target> <version> <bin dir> <out dir>
# Writes <out dir>/<apk arch>/cpxy-<version>.apk without rebuilding the binaries.
set -eu

[ $# -eq 4 ] || { sed -n '2,4p' "$0" >&2; exit 2; }
target="$1" version="$2" bindir="$3" outdir="$4"
here="$(cd "$(dirname "$0")" && pwd)"
arches="$("$here/apk-architectures.sh" "$target")"
for arch in $arches; do
	"$here/build-apk.sh" "$arch" "$version" "$bindir" "$outdir/$arch"
done
