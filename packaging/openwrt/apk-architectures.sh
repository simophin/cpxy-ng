#!/bin/sh
# Package labels for each generic static binary target. Keep the plain aarch64
# variant for systems accepting that label; OpenWrt uses the more specific names.
# OpenWrt 25.12: https://downloads.openwrt.org/releases/25.12.0/packages/
set -eu

[ $# -eq 1 ] || { echo "usage: $0 <rust target | --all>" >&2; exit 2; }
case "$1" in
aarch64-unknown-linux-musl) echo 'aarch64 aarch64_generic aarch64_cortex-a53 aarch64_cortex-a72 aarch64_cortex-a76' ;;
x86_64-unknown-linux-musl) echo x86_64 ;;
--all)
	"$0" aarch64-unknown-linux-musl
	"$0" x86_64-unknown-linux-musl
	;;
*) echo "unsupported binary target: $1" >&2; exit 2 ;;
esac
