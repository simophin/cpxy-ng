#!/bin/sh
# The cpxy-dns-split service: the split resolver, and (DNS_SYSTEM=1) handing it the box's DNS.
#
#   dns.sh run        exec cpxy-dns-split
#   dns.sh takeover   once it listens, point /etc/resolv.conf at it (ExecStartPost)
#   dns.sh restore    put the original /etc/resolv.conf back (ExecStopPost)
#
# The box only ever points at a resolver that is listening, and gets its own DNS back whenever the
# resolver stops, crashes included: a broken resolver costs the split, not DNS.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
. "$here/config.sh"
CPXY_BIN="${CPXY_BIN:-/usr/bin}"
RESOLV_CONF="${CPXY_RESOLV_CONF:-/etc/resolv.conf}"
STATE="${CPXY_STATE:-/var/lib/cpxy}"
BACKUP="$STATE/resolv.conf.orig"
MARKER="# Managed by cpxy-dns-split; the original is restored when it stops."

cpxy_load_config
host="${CPXY_DNS_LISTEN%:*}"
port="${CPXY_DNS_LISTEN##*:}"

_check_config() {
	[ -n "$CPXY_DNS_UPSTREAM" ] || cpxy_die "DNS_UPSTREAM is not set in $CPXY_CONF"
	[ -n "$CPXY_DNS_ALTERNATIVE" ] || cpxy_die "DNS_ALTERNATIVE is not set in $CPXY_CONF"
	_cpxy_uint "$port" 1 65535 || cpxy_die "DNS_LISTEN must be <ip>:<port>"
	if [ "$CPXY_DNS_SYSTEM" = 1 ]; then
		[ "$port" = 53 ] || cpxy_die "DNS_SYSTEM=1 needs DNS_LISTEN on port 53"
		case "$host" in *[!0-9.]* | '') cpxy_die "DNS_SYSTEM=1 needs DNS_LISTEN on an IPv4 address" ;; esac
	fi
}

_ours() { [ -f "$RESOLV_CONF" ] && [ ! -L "$RESOLV_CONF" ] && grep -qxF "$MARKER" "$RESOLV_CONF"; }

# Tailscale with "accept DNS" on rewrites resolv.conf for itself, replacing the split resolver.
_tailscale_accept_dns_off() {
	[ "$CPXY_TAILSCALE_ACCEPT_DNS_OFF" = 1 ] || return 0
	command -v tailscale >/dev/null 2>&1 || return 0
	if tailscale debug prefs 2>/dev/null | grep -q '"CorpDNS": *true'; then
		if tailscale set --accept-dns=false; then
			cpxy_log "turned off Tailscale's accept-dns on this box so it keeps using cpxy-dns-split"
		else
			cpxy_log "warning: could not turn off Tailscale's accept-dns; it may replace /etc/resolv.conf"
		fi
	fi
}

# Both UDP and TCP, as resolv.conf clients use both. By address: systemd-resolved has :53 on
# 127.0.0.53 and 127.0.0.54.
_listening() {
	ss -Hln -u src "$CPXY_DNS_LISTEN" | grep -q . && ss -Hln -t src "$CPXY_DNS_LISTEN" | grep -q .
}

case "${1:-}" in
	run)
		_check_config
		set -- --listen "$CPXY_DNS_LISTEN"
		for s in $(printf '%s' "$CPXY_DNS_UPSTREAM" | tr ',' ' '); do set -- "$@" --upstream "$s"; done
		for s in $(printf '%s' "$CPXY_DNS_ALTERNATIVE" | tr ',' ' '); do set -- "$@" --alternative "$s"; done
		if [ "$CPXY_DNS_CACHE" = 1 ]; then
			set -- "$@" --cache-db "${CACHE_DIRECTORY:-/var/cache/cpxy}/dns-cache.sqlite"
		else
			set -- "$@" --no-cache
		fi
		export NO_COLOR=1
		exec "$CPXY_BIN/cpxy-dns-split" "$@"
		;;
	takeover)
		[ "$CPXY_DNS_SYSTEM" = 1 ] || exit 0
		_tailscale_accept_dns_off
		i=0
		until _listening; do
			i=$((i + 1))
			[ "$i" -le 50 ] || cpxy_die "cpxy-dns-split is not listening on $CPXY_DNS_LISTEN; leaving $RESOLV_CONF alone"
			sleep 0.2
		done
		_ours && exit 0
		mkdir -p "$STATE"
		if [ -e "$RESOLV_CONF" ] || [ -L "$RESOLV_CONF" ]; then
			# -P keeps a symlink (systemd-resolved's stub, for one) a symlink
			cp -P "$RESOLV_CONF" "$BACKUP.tmp" && mv -f "$BACKUP.tmp" "$BACKUP"
		else
			rm -f "$BACKUP"
		fi
		printf '%s\nnameserver %s\noptions edns0 trust-ad\n' "$MARKER" "$host" >"$RESOLV_CONF.cpxy"
		mv -f "$RESOLV_CONF.cpxy" "$RESOLV_CONF"
		cpxy_log "$RESOLV_CONF now points at cpxy-dns-split ($host)"
		;;
	restore)
		_ours || exit 0
		if [ -e "$BACKUP" ] || [ -L "$BACKUP" ]; then
			mv -f "$BACKUP" "$RESOLV_CONF"
		else
			rm -f "$RESOLV_CONF"
		fi
		cpxy_log "restored the original $RESOLV_CONF"
		;;
	*)
		echo "usage: $0 run|takeover|restore" >&2
		exit 2
		;;
esac
