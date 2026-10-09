#!/bin/sh
# shellcheck disable=SC2034 # the CPXY_ variables are read by the scripts that source this
# Reads /etc/cpxy/cpxy.conf into CPXY_<KEY> variables. The file is parsed, never sourced: it holds
# the server's secret key and values such as URLs with ? and &.

CPXY_CONF="${CPXY_CONF:-/etc/cpxy/cpxy.conf}"

cpxy_log() { echo "cpxy: $*" >&2; }
cpxy_die() { cpxy_log "$*"; exit 1; }

cpxy_load_config() {
	local line key value
	CPXY_SERVER=""
	CPXY_INTERFACES=tailscale0
	CPXY_MTU=1280
	CPXY_TUN=cpxy0
	CPXY_ROUTING_TABLE=100
	CPXY_RULE_PRIORITY=9100
	CPXY_TUN_ADDRESS=198.18.0.1/30
	CPXY_DNS_UPSTREAM=""
	CPXY_DNS_ALTERNATIVE=""
	CPXY_DNS_SYSTEM=1
	CPXY_DNS_LISTEN=127.0.0.1:53
	CPXY_TAILSCALE_ACCEPT_DNS_OFF=1
	CPXY_DNS_CACHE=1
	[ -r "$CPXY_CONF" ] || cpxy_die "cannot read $CPXY_CONF"
	while IFS= read -r line || [ -n "$line" ]; do
		case "$line" in '' | '#'*) continue ;; esac
		key="${line%%=*}"
		value="${line#*=}"
		[ "$key" != "$line" ] || cpxy_die "$CPXY_CONF: not KEY=value: $line"
		# Trim surrounding blanks and one pair of quotes
		key="$(printf '%s' "$key" | tr -d ' \t')"
		value="$(printf '%s' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
			-e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/")"
		case "$key" in
			SERVER | INTERFACES | MTU | TUN | ROUTING_TABLE | RULE_PRIORITY | TUN_ADDRESS | \
				DNS_UPSTREAM | DNS_ALTERNATIVE | DNS_SYSTEM | DNS_LISTEN | TAILSCALE_ACCEPT_DNS_OFF | DNS_CACHE)
				eval "CPXY_$key=\$value" ;;
			*) cpxy_log "$CPXY_CONF: ignoring unknown setting $key" ;;
		esac
	done <"$CPXY_CONF"
	CPXY_INTERFACES="$(printf '%s' "$CPXY_INTERFACES" | tr ',' ' ')"
}

_cpxy_uint() { # _cpxy_uint <value> <min> <max>
	case "$1" in '' | *[!0-9]* | 0[0-9]*) return 1 ;; esac
	[ "$1" -ge "$2" ] && [ "$1" -le "$3" ]
}

cpxy_check_routing_config() {
	local dev
	[ -n "$CPXY_SERVER" ] || cpxy_die "SERVER is not set in $CPXY_CONF"
	[ -n "$CPXY_INTERFACES" ] || cpxy_die "INTERFACES is empty in $CPXY_CONF"
	for dev in $CPXY_INTERFACES; do
		case "$dev" in *[!A-Za-z0-9_.@-]*) cpxy_die "invalid interface name: $dev" ;; esac
		[ "$dev" != "$CPXY_TUN" ] || cpxy_die "INTERFACES must not include the TUN ($CPXY_TUN)"
	done
	case "$CPXY_TUN" in '' | *[!A-Za-z0-9_-]* | lo) cpxy_die "invalid TUN: $CPXY_TUN" ;; esac
	[ "${#CPXY_TUN}" -le 15 ] || cpxy_die "TUN exceeds 15 characters"
	_cpxy_uint "$CPXY_MTU" 1280 9000 || cpxy_die "MTU must be 1280..9000"
	if ! _cpxy_uint "$CPXY_ROUTING_TABLE" 1 65535 || _cpxy_uint "$CPXY_ROUTING_TABLE" 253 255; then
		cpxy_die "ROUTING_TABLE must be 1..65535, excluding 253..255"
	fi
	_cpxy_uint "$CPXY_RULE_PRIORITY" 1 32762 || cpxy_die "RULE_PRIORITY must be 1..32762"
}
