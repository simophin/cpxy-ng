#!/bin/sh
# Temporary forwarding barrier, run by the firewall as an include and by cpxy-router after each
# apply. nftables state does not survive a reboot, so from boot until cpxy-router installs the
# `inet cpxy` table, the routed networks' devices would forward everything to the WAN. This
# rejects all forwarding from each device the table does not guard yet.
#
# On fw3 the policy is iptables chains (ipt.sh) and the barrier is the cpxy_boot_guard chain. A
# fw3 reload may flush them, so this first restores the last applied rules.
. /lib/functions.sh
. /usr/libexec/cpxy/instance.sh
. /usr/libexec/cpxy/nft.sh
. /usr/libexec/cpxy/ipt.sh

FW3=0
_cpxy_have_fw4 || ! command -v iptables >/dev/null || FW3=1

clear_guard() {
	local fam chain
	if [ "$FW3" = 1 ]; then
		# CPXY_BOOT is the TUN design's barrier
		for fam in 4 6; do
			for chain in cpxy_boot_guard CPXY_BOOT; do
				while _cpxy_ipt "$fam" -D FORWARD -j "$chain" 2>/dev/null; do :; done
				_cpxy_ipt "$fam" -F "$chain" 2>/dev/null
				_cpxy_ipt "$fam" -X "$chain" 2>/dev/null
			done
		done
		return 0
	fi
	nft delete table inet cpxy_boot_guard 2>/dev/null || :
}

guarded() {
	if [ "$FW3" = 1 ]; then cpxy_ipt_guarded "$1"; else cpxy_nft_guarded "$1"; fi
}

[ "${1:-}" != clear ] || { clear_guard; exit 0; }
active=0
for link in /etc/rc.d/S??cpxy-router; do [ ! -L "$link" ] || active=1; done
if [ "$active" != 1 ] || [ -f "$CPXY_RUN/router-stopped" ]; then clear_guard; exit 0; fi
[ "$FW3" != 1 ] || cpxy_ipt_reapply || echo "cpxy: could not restore the iptables policy" >&2

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
		guarded "$dev" || printf '%s\n' "$dev" >>"$work/devices"
	done
done

if [ ! -s "$work/devices" ]; then clear_guard; exit 0; fi
if [ "$FW3" = 1 ]; then
	for fam in 4 6; do
		_cpxy_ipt "$fam" -N cpxy_boot_guard 2>/dev/null
		_cpxy_ipt "$fam" -F cpxy_boot_guard || exit 1
		while read -r dev; do
			_cpxy_ipt "$fam" -A cpxy_boot_guard -i "$dev" -j REJECT || exit 1
		done <"$work/devices"
		_cpxy_ipt "$fam" -C FORWARD -j cpxy_boot_guard 2>/dev/null ||
			_cpxy_ipt "$fam" -I FORWARD 1 -j cpxy_boot_guard || exit 1
	done
	exit 0
fi
{
	echo 'add table inet cpxy_boot_guard'
	echo 'add chain inet cpxy_boot_guard forward { type filter hook forward priority -10; policy accept; }'
	echo 'flush chain inet cpxy_boot_guard forward'
	while read -r dev; do
		printf 'add rule inet cpxy_boot_guard forward iifname "%s" counter reject\n' "$dev"
	done <"$work/devices"
} >"$work/guard.nft" || exit 1
nft -f "$work/guard.nft"
