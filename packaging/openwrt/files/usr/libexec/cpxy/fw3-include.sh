#!/bin/sh
# fw3 include (firewalls without fw4, such as GL.iNet's OpenWrt 21.02-based firmware), registered by
# /etc/uci-defaults/90-cpxy as firewall.cpxy_udp and run on every firewall start and reload.
# The iptables counterpart of the nftables snippet that fw4 includes: cpxy has no UDP path yet, so
# refuse the UDP routed into the cpxy TUN device (QUIC, UDP 443) at once and let it fall back to TCP.
iptables -C FORWARD -o cpxy0 -p udp -j REJECT 2>/dev/null ||
	iptables -I FORWARD -o cpxy0 -p udp -j REJECT
exit 0
