#!/bin/sh
# Temporary forwarding barrier, run by the firewall as an include and by cpxy-router after each
# apply. nftables state does not survive a reboot, so from boot until cpxy-router installs the
# `inet cpxy` table, the routed networks' devices would forward everything to the WAN. This
# rejects all forwarding from each device the table does not guard yet.
. /lib/functions.sh
. /usr/libexec/cpxy/instance.sh
. /usr/libexec/cpxy/nft.sh

clear_guard() {
	nft delete table inet cpxy_boot_guard 2>/dev/null || :
}

[ "${1:-}" != clear ] || { clear_guard; exit 0; }
active=0
for link in /etc/rc.d/S??cpxy-router; do [ ! -L "$link" ] || active=1; done
if [ "$active" != 1 ] || [ -f "$CPXY_RUN/router-stopped" ]; then clear_guard; exit 0; fi

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
		[ "$kind" = instance ] && [ "$enabled$CPXY_ROUTED" = 11 ] || continue
	fi
	{ read -r format; read -r devices; } <"$state"
	# A snapshot from the TUN design lists its devices on the fourth line
	[ "$format" = nft ] || devices="$(sed -n '4p' "$state")"
	for dev in $devices; do
		_cpxy_nft_dev_ok "$dev" || exit 1
		cpxy_nft_guarded "$dev" || printf '%s\n' "$dev" >>"$work/devices"
	done
done

if [ ! -s "$work/devices" ]; then clear_guard; exit 0; fi
{
	echo 'add table inet cpxy_boot_guard'
	echo 'add chain inet cpxy_boot_guard forward { type filter hook forward priority -10; policy accept; }'
	echo 'flush chain inet cpxy_boot_guard forward'
	while read -r dev; do
		printf 'add rule inet cpxy_boot_guard forward iifname "%s" counter reject\n' "$dev"
	done <"$work/devices"
} >"$work/guard.nft" || exit 1
nft -f "$work/guard.nft"
