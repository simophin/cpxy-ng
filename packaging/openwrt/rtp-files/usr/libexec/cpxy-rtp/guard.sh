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
	if [ -s "$P/routes6" ]; then
		printf 'add set inet cpxy_rtp_guard direct6 { type ipv6_addr; flags interval; auto-merge; elements = { '
		awk '{printf "%s%s", (NR==1?"":","),$0}' "$P/routes6"
		printf ' }; }\n'
	fi
	printf 'add chain inet cpxy_rtp_guard input { type filter hook input priority -5; policy accept; }\n'
	printf 'add chain inet cpxy_rtp_guard forward { type filter hook forward priority -5; policy accept; }\n'
	for dev in $(cat "$P/devices"); do
		case "$dev" in ''|*[!A-Za-z0-9_.:-]*) exit 1;; esac
		printf 'add rule inet cpxy_rtp_guard input iifname "%s" meta nfproto ipv6 meta l4proto { tcp, udp } th dport 53 reject\n' "$dev"
		if [ -s "$P/routes6" ]; then
			# DNS stays on the IPv4 split listener; literal/custom-resolver clients
			# still encounter these forwarding guards.
			printf 'add rule inet cpxy_rtp_guard forward iifname "%s" meta nfproto ipv6 meta l4proto { tcp, udp } th dport 53 reject with icmpv6 type admin-prohibited\n' "$dev"
			printf 'add rule inet cpxy_rtp_guard forward iifname "%s" ip6 daddr @direct6 return\n' "$dev"
			printf 'add rule inet cpxy_rtp_guard forward iifname "%s" meta nfproto ipv6 meta l4proto tcp reject with tcp reset\n' "$dev"
			printf 'add rule inet cpxy_rtp_guard forward iifname "%s" meta nfproto ipv6 reject with icmpv6 type admin-prohibited\n' "$dev"
		else
			printf 'add rule inet cpxy_rtp_guard forward iifname "%s" meta nfproto ipv6 reject\n' "$dev"
		fi
		printf 'add rule inet cpxy_rtp_guard forward iifname "%s" oifname != "rtpr0" ip daddr != @direct reject\n' "$dev"
	done
} >"$work"
nft -f "$work"
