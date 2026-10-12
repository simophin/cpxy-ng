#!/bin/sh
# Instance settings, shared by the cpxy-* init scripts. Each `config instance '<name>'` section in
# /etc/config/cpxy is one bundle for one set of networks. A setting is the instance's own option,
# else the `defaults` section's, else built in. Ports, devices and routing numbers are derived
# from the instance's `index` unless the instance sets them itself.
#
# Callers source /lib/functions.sh and this file, then run cpxy_validate (which loads the config)
# and cpxy_foreach.
# The CPXY_* variables are for the scripts that source this.
# shellcheck disable=SC2034

CPXY_BIN=/usr/bin
CPXY_RUN=/tmp/cpxy
# Names on the ad blocklist resolve here (dns_split --sinkhole); cpxy-router drops the range.
# Not a private range: dnsmasq's rebind protection would discard those answers. Kept in step
# with mobile-engine/src/config.rs.
CPXY_SINKHOLE_ADDR=198.18.0.1
CPXY_SINKHOLE_NET=198.18.0.0/24

cpxy_log() {
	logger -t cpxy -p daemon.err "$*"
	echo "cpxy: $*" >&2
}

# cpxy_get <var> <instance> <option> [default]: the instance's value, else the defaults section's.
# A list comes back space-separated.
cpxy_get() {
	local _cpxy_v
	config_get _cpxy_v "$2" "$3"
	[ -n "$_cpxy_v" ] || config_get _cpxy_v defaults "$3"
	[ -n "$_cpxy_v" ] || _cpxy_v="$4"
	eval "$1=\$_cpxy_v"
}

# cpxy_get_bool <var> <instance> <option> <default>
cpxy_get_bool() {
	local _cpxy_b
	cpxy_get _cpxy_b "$2" "$3" "$4"
	case "$_cpxy_b" in
	1 | on | true | yes | enabled) _cpxy_b=1 ;;
	*) _cpxy_b=0 ;;
	esac
	eval "$1=\$_cpxy_b"
}

# A resource that belongs to one instance: never taken from the defaults section.
_cpxy_own() {
	local _cpxy_v
	config_get _cpxy_v "$2" "$3"
	eval "$1=\${_cpxy_v:-\$4}"
}

_cpxy_port() { echo "${1##*:}"; }

# Where a local client reaches a listener: its own address, or loopback for a wildcard one.
_cpxy_connect_addr() {
	local host="${1%:*}"
	case "$host" in '' | '[::]' | 0.0.0.0) host=127.0.0.1 ;; esac
	echo "$host:$(_cpxy_port "$1")"
}

_cpxy_uint() {
	case "$1" in '' | *[!0-9]* | 0[0-9]*) return 1 ;; esac
	[ "$1" -ge "$2" ] 2>/dev/null && [ "$1" -le "$3" ] 2>/dev/null
}

# cpxy_instance <name>: loads one instance's settings into CPXY_* variables.
cpxy_instance() {
	local i name="$1" domain
	config_get CPXY_INDEX "$name" index
	i="${CPXY_INDEX:-0}"
	_cpxy_uint "$i" 0 9 || i=0

	cpxy_get CPXY_SERVER "$name" server
	cpxy_get CPXY_NETWORKS "$name" network
	cpxy_get_bool CPXY_CLIENT "$name" client 1
	cpxy_get_bool CPXY_SMARTDNS "$name" smartdns 1
	cpxy_get_bool CPXY_ROUTER "$name" router 1
	cpxy_get_bool CPXY_TSGW "$name" tsgw 1

	_cpxy_own CPXY_HTTP_LISTEN "$name" client_http_listen "[::]:$((5000 + i))"
	cpxy_get_bool CPXY_SOCKS5 "$name" client_socks5 0
	_cpxy_own CPXY_SOCKS5_LISTEN "$name" client_socks5_listen "[::]:$((1080 + i))"
	CPXY_PROXY="http://$(_cpxy_connect_addr "$CPXY_HTTP_LISTEN")"

	_cpxy_own CPXY_SMARTDNS_LISTEN "$name" smartdns_listen "127.0.0.1:$((5336 + 10 * i))"
	cpxy_get CPXY_SMARTDNS_SERVERS "$name" smartdns_server
	cpxy_get_bool CPXY_SMARTDNS_PROXY "$name" smartdns_proxy 1
	cpxy_get CPXY_SMARTDNS_CACHE_SIZE "$name" smartdns_cache_size 4096
	cpxy_get_bool CPXY_SMARTDNS_PREFETCH "$name" smartdns_prefetch 1
	cpxy_get_bool CPXY_SMARTDNS_SERVE_EXPIRED "$name" smartdns_serve_expired 1

	cpxy_get CPXY_DNS_UPSTREAM "$name" dns_upstream
	cpxy_get CPXY_DNS_ALTERNATIVE "$name" dns_alternative
	if [ -z "$CPXY_DNS_ALTERNATIVE" ] && [ "$CPXY_SMARTDNS" = 1 ]; then
		CPXY_DNS_ALTERNATIVE="$(_cpxy_connect_addr "$CPXY_SMARTDNS_LISTEN")"
	fi
	cpxy_get_bool CPXY_DNS_CACHE "$name" dns_cache 1
	cpxy_get_bool CPXY_BLOCK_ADS "$name" block_ads 1
	cpxy_get CPXY_DNS_LOCAL_DOMAINS "$name" dns_local_domain
	if [ -z "$CPXY_DNS_LOCAL_DOMAINS" ]; then
		# dnsmasq's own domains: its DHCP domain and local=/lan/
		for domain in $(uci -q get 'dhcp.@dnsmasq[0].domain') $(uci -q get 'dhcp.@dnsmasq[0].local' | tr / ' '); do
			case " $CPXY_DNS_LOCAL_DOMAINS " in
			*" $domain "*) ;;
			*) CPXY_DNS_LOCAL_DOMAINS="${CPXY_DNS_LOCAL_DOMAINS:+$CPXY_DNS_LOCAL_DOMAINS }$domain" ;;
			esac
		done
	fi
	cpxy_get CPXY_DNS_LOCAL_SERVER "$name" dns_local_server 127.0.0.1:53

	cpxy_get_bool CPXY_ROUTER_DNS "$name" router_dns 1
	_cpxy_own CPXY_TUN "$name" tun "cpxy$i"
	_cpxy_own CPXY_TABLE "$name" routing_table "$((100 + i))"
	_cpxy_own CPXY_PRIORITY "$name" rule_priority "$((9100 + 10 * i))"
	_cpxy_own CPXY_DNS_LISTEN "$name" dns_listen "0.0.0.0:$((5335 + 10 * i))"

	_cpxy_own CPXY_TSGW_HOSTNAME "$name" tsgw_hostname "cpxy-$(echo "$name" | tr 'A-Z_' 'a-z-')"
	_cpxy_own CPXY_TSGW_PORT "$name" tsgw_port "$((41641 + i))"
	_cpxy_own CPXY_TSGW_STATE_DIR "$name" tsgw_state_dir "/etc/cpxy/tsgw/$name"
	cpxy_get CPXY_TSGW_AUTHKEY "$name" tsgw_authkey
	cpxy_get CPXY_TSGW_CONTROL_URL "$name" tsgw_control_url
	cpxy_get CPXY_TSGW_MTU "$name" tsgw_mtu 1280
	cpxy_get_bool CPXY_TSGW_PROXY "$name" tsgw_proxy 1
	cpxy_get_bool CPXY_TSGW_NO_LOGS "$name" tsgw_no_logs_no_support 0
	cpxy_get_bool CPXY_TSGW_VERBOSE "$name" tsgw_verbose 0
}

# Whether a smartdns server entry can be used: an IP (no lookup through the system resolver,
# which may lead back here), and over TCP when it goes through the HTTP proxy.
_cpxy_smartdns_server_ok() {
	local entry="$1" proxy="$2" scheme rest host
	case "$entry" in
	*://*) scheme="${entry%%://*}" rest="${entry#*://}" ;;
	*) scheme=udp rest="$entry" ;;
	esac
	case "$scheme" in
	tcp | tls | https) ;;
	udp) [ "$proxy" = 0 ] || return 1 ;;
	*) return 1 ;;
	esac
	host="${rest%%[/?]*}"
	case "$host" in
	\[*\]* ) return 0 ;;
	esac
	host="${host%:*}"
	case "$host" in '' | *[!0-9.]*) return 1 ;; esac
}

_cpxy_claim() {
	# _cpxy_claim <kind> <value> <what>: each value of a kind may be claimed once
	local kind="$1" value="$2" what="$3" seen
	eval "seen=\"\$_CPXY_CLAIMED_$kind\""
	case " $seen " in
	*" $value "*) _cpxy_invalid "$what: $kind $value is already used by another instance or component" ;;
	esac
	eval "_CPXY_CLAIMED_$kind=\"\$seen \$value\""
}

_cpxy_invalid() {
	cpxy_log "$*"
	CPXY_VALID=0
}

_cpxy_validate_instance() {
	local enabled entry
	config_get_bool enabled "$1" enabled 1
	[ "$enabled" = 1 ] || return 0
	case "$1" in
	'' | *[!A-Za-z0-9_]*)
		_cpxy_invalid "instance '$1': use a name of letters, digits and underscores"
		return
		;;
	esac
	[ "${#1}" -le 32 ] || { _cpxy_invalid "instance '$1': the name is longer than 32 characters"; return; }
	config_get CPXY_INDEX "$1" index
	_cpxy_uint "$CPXY_INDEX" 0 9 || { _cpxy_invalid "$1: option 'index' must be 0..9"; return; }
	_cpxy_claim index "$CPXY_INDEX" "$1"
	cpxy_instance "$1"

	if [ "$CPXY_CLIENT$CPXY_ROUTER$CPXY_TSGW" != 000 ] && [ -z "$CPXY_SERVER" ]; then
		_cpxy_invalid "$1: option 'server' is not set (in the instance or in defaults)"
	fi
	if [ "$CPXY_CLIENT" = 1 ]; then
		_cpxy_claim tcp "$(_cpxy_port "$CPXY_HTTP_LISTEN")" "$1 client_http_listen"
		[ "$CPXY_SOCKS5" = 1 ] && _cpxy_claim tcp "$(_cpxy_port "$CPXY_SOCKS5_LISTEN")" "$1 client_socks5_listen"
	fi
	if [ "$CPXY_SMARTDNS" = 1 ]; then
		case "$(_cpxy_port "$CPXY_SMARTDNS_LISTEN")" in
		53 | 6053) _cpxy_invalid "$1: smartdns_listen must not use port 53 or 6053, the system resolvers' ports" ;;
		esac
		_cpxy_claim tcp "$(_cpxy_port "$CPXY_SMARTDNS_LISTEN")" "$1 smartdns_listen"
		[ -n "$CPXY_SMARTDNS_SERVERS" ] || _cpxy_invalid "$1: option 'smartdns_server' is required"
		for entry in $CPXY_SMARTDNS_SERVERS; do
			_cpxy_smartdns_server_ok "$entry" "$CPXY_SMARTDNS_PROXY" ||
				_cpxy_invalid "$1: smartdns_server '$entry' must be an IP address, over tcp://, tls:// or https:// through the proxy"
		done
		[ "$CPXY_SMARTDNS_PROXY" = 0 ] || [ "$CPXY_CLIENT" = 1 ] ||
			_cpxy_invalid "$1: smartdns_proxy needs this instance's client"
		_cpxy_uint "$CPXY_SMARTDNS_CACHE_SIZE" 0 1048576 || _cpxy_invalid "$1: smartdns_cache_size must be a number"
	fi
	if [ "$CPXY_ROUTER" = 1 ] || [ "$CPXY_TSGW" = 1 ]; then
		[ -n "$CPXY_DNS_UPSTREAM" ] || _cpxy_invalid "$1: option 'dns_upstream' is required"
		[ -n "$CPXY_DNS_ALTERNATIVE" ] ||
			_cpxy_invalid "$1: option 'dns_alternative' is required when the instance's smartdns is off"
	fi
	if [ "$CPXY_ROUTER" = 1 ] && [ "$CPXY_ROUTER_DNS" = 1 ]; then
		case "$(_cpxy_port "$CPXY_DNS_LISTEN")" in
		53) _cpxy_invalid "$1: dns_listen must not use port 53, dnsmasq's" ;;
		esac
		_cpxy_claim tcp "$(_cpxy_port "$CPXY_DNS_LISTEN")" "$1 dns_listen"
	fi
	if [ "$CPXY_TSGW" = 1 ]; then
		_cpxy_uint "$CPXY_TSGW_PORT" 0 65535 || _cpxy_invalid "$1: tsgw_port must be 0..65535"
		[ "$CPXY_TSGW_PORT" = 0 ] || _cpxy_claim udp "$CPXY_TSGW_PORT" "$1 tsgw_port"
		_cpxy_claim hostname "$CPXY_TSGW_HOSTNAME" "$1 tsgw_hostname"
		_cpxy_claim state "$CPXY_TSGW_STATE_DIR" "$1 tsgw_state_dir"
		[ "$CPXY_TSGW_PROXY" = 0 ] || [ "$CPXY_CLIENT" = 1 ] ||
			_cpxy_invalid "$1: tsgw_proxy needs this instance's client"
	fi
}

# cpxy_validate: loads /etc/config/cpxy and checks every enabled instance together, so a
# conflict between two instances stops every service from applying it.
cpxy_validate() {
	CPXY_VALID=1
	_CPXY_CLAIMED_index="" _CPXY_CLAIMED_tcp="" _CPXY_CLAIMED_udp=""
	_CPXY_CLAIMED_hostname="" _CPXY_CLAIMED_state=""
	config_load cpxy
	config_foreach _cpxy_validate_instance instance
	[ "$CPXY_VALID" = 1 ]
}

_cpxy_foreach_cb() {
	local enabled
	config_get_bool enabled "$1" enabled 1
	[ "$enabled" = 1 ] || return 0
	cpxy_instance "$1"
	"$_CPXY_FOREACH" "$1"
}

# cpxy_foreach <callback>: runs callback <name> with CPXY_* set, for each enabled instance.
cpxy_foreach() {
	_CPXY_FOREACH="$1"
	config_foreach _cpxy_foreach_cb instance
}
