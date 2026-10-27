#!/bin/sh
# Independent fw4-reload/boot guard: only direct prefixes may leave outside rtpr0.
set -eu
P=/etc/cpxy-rtp
[ -f "$P/devices" ] && [ -f "$P/routes" ] || exit 0
work="$(mktemp)"; trap 'rm -f "$work"' EXIT
{
	printf 'add table inet cpxy_rtp_guard\nflush table inet cpxy_rtp_guard\n'
	printf 'add set inet cpxy_rtp_guard direct { type ipv4_addr; flags interval; auto-merge; elements = { '
	awk '{printf "%s%s", (NR==1?"":","),$0}' "$P/routes"
	printf ' }; }\n'
	printf 'add chain inet cpxy_rtp_guard input { type filter hook input priority -5; policy accept; }\n'
	printf 'add chain inet cpxy_rtp_guard forward { type filter hook forward priority -5; policy accept; }\n'
	for dev in $(cat "$P/devices"); do
		case "$dev" in ''|*[!A-Za-z0-9_.:-]*) exit 1;; esac
		printf 'add rule inet cpxy_rtp_guard input iifname "%s" meta nfproto ipv6 meta l4proto { tcp, udp } th dport 53 reject\n' "$dev"
		printf 'add rule inet cpxy_rtp_guard forward iifname "%s" meta nfproto ipv6 reject\n' "$dev"
		printf 'add rule inet cpxy_rtp_guard forward iifname "%s" oifname != "rtpr0" ip daddr != @direct reject\n' "$dev"
	done
} >"$work"
nft -f "$work"
