#!/bin/sh
# Temporary forwarding barrier, not GeoIP classification. netifd flushes policy rules at
# startup, so a routing-only early guard cannot protect the interval before route setup.
. /lib/functions.sh
. /usr/libexec/cpxy/instance.sh

clear_iptables() {
	local tool
	for tool in iptables ip6tables; do
		command -v "$tool" >/dev/null || continue
		while "$tool" -w -D FORWARD -j CPXY_BOOT 2>/dev/null; do :; done
		"$tool" -w -F CPXY_BOOT 2>/dev/null
		"$tool" -w -X CPXY_BOOT 2>/dev/null
	done
	return 0
}

clear_guard() {
	if command -v fw4 >/dev/null; then
		nft delete table inet cpxy_boot_guard 2>/dev/null || :
	else
		clear_iptables
	fi
}

[ "${1:-}" != clear ] || { clear_guard; exit 0; }
active=0
for link in /etc/rc.d/S??cpxy-router; do [ ! -L "$link" ] || active=1; done
if [ "$active" != 1 ] || [ -f "$CPXY_RUN/geo-stopped" ]; then clear_guard; exit 0; fi

work="$(mktemp -d /tmp/cpxy-boot-fw.XXXXXX)" || exit 1
trap 'rm -rf "$work"' EXIT
: >"$work/devices"
loaded=1
config_load cpxy || loaded=0
for state in /etc/cpxy/router-guards/*; do
	[ -f "$state" ] || continue
	name="${state##*/}"
	if [ "$loaded" = 1 ]; then
		kind='' enabled=0
		config_get kind "$name" TYPE
		config_get_bool enabled "$name" enabled 1
		cpxy_instance "$name"
		[ "$kind" = instance ] && [ "$enabled$CPXY_ROUTER$CPXY_GEOIP" = 111 ] && [ -n "$CPXY_NETWORKS" ] || continue
	fi
	{ read -r _tun; read -r table; read -r priority; read -r devices; } <"$state"
	_cpxy_uint "$priority" 1 32761 && _cpxy_uint "$table" 1 55535 || exit 1
	for dev in $devices; do
		# A complete capture selector plus both terminal guards safely handles even a
		# missing table. During boot netifd removes these, while this barrier survives.
		protected=1
		for fam in -4 -6; do
			ip "$fam" rule show priority "$((priority + 3))" | grep -qF "iif $dev ipproto tcp lookup " || protected=0
			ip "$fam" rule show priority "$priority" | grep -qF "iif $dev ipproto udp dport 443 prohibit" || protected=0
			ip "$fam" rule show priority "$((priority + 4))" | grep -qF "iif $dev " || protected=0
		done
		[ "$protected" = 1 ] || printf '%s\n' "$dev" >>"$work/devices"
	done
done

if [ ! -s "$work/devices" ]; then clear_guard; exit 0; fi
if command -v fw4 >/dev/null; then
	{
		echo 'add table inet cpxy_boot_guard'
		echo 'add chain inet cpxy_boot_guard forward { type filter hook forward priority -10; policy accept; }'
		echo 'flush chain inet cpxy_boot_guard forward'
		while read -r dev; do
			case "$dev" in '' | *[!A-Za-z0-9_.:@-]*) exit 1 ;; esac
			printf 'add rule inet cpxy_boot_guard forward iifname "%s" counter reject\n' "$dev"
		done <"$work/devices"
	} >"$work/guard.nft" || exit 1
	nft -f "$work/guard.nft"
else
	# fw3 invokes script includes after restoring its tables. Put our chain before
	# established/related acceptance, and rebuild it on every firewall reload.
	for tool in iptables ip6tables; do
		command -v "$tool" >/dev/null || exit 1
		"$tool" -w -N CPXY_BOOT 2>/dev/null || "$tool" -w -S CPXY_BOOT >/dev/null || exit 1
		"$tool" -w -F CPXY_BOOT || exit 1
		while read -r dev; do "$tool" -w -A CPXY_BOOT -i "$dev" -j REJECT || exit 1; done <"$work/devices"
		"$tool" -w -C FORWARD -j CPXY_BOOT 2>/dev/null || "$tool" -w -I FORWARD 1 -j CPXY_BOOT || exit 1
	done
fi
