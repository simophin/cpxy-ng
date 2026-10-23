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

# API listeners accept numeric IPv4 or bracketed IPv6 addresses, just like Rust SocketAddr.
_cpxy_api_address() {
	local host="${1%:*}" port="${1##*:}"
	_cpxy_uint "$port" 1 65535 || return 1
	case "$host" in
	\[*\]) host="${host#\[}"; host="${host%\]}"; _cpxy_ipv6_cidr "$host/128" ;;
	*) _cpxy_ipv4_cidr "$host/32" ;;
	esac
}

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

# Validate before a reload changes running workers or kernel routing.
_cpxy_ipv4_cidr() {
	printf '%s\n' "$1" | awk -F '[./]' '
		$0 !~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/[0-9]+$/ { exit 1 }
		{
			for (i = 1; i <= 4; i++) {
				if ($i !~ /^[0-9]+$/ || length($i) > 3 || $i > 255 ||
				    (length($i) > 1 && substr($i, 1, 1) == "0")) exit 1
			}
			if ($5 !~ /^[0-9]+$/ || $5 > 32) exit 1
		}'
}

_cpxy_ipv6_cidr() {
	printf '%s\n' "$1" | awk -F / '
		NF != 2 || $2 !~ /^[0-9]+$/ || $2 > 128 || (length($2) > 1 && substr($2, 1, 1) == "0") { exit 1 }
		{
			addr = $1
			if (addr !~ /^[0-9A-Fa-f:]+$/ || addr ~ /:::/ || addr ~ /^:[^:]/ || addr ~ /[^:]:$/) exit 1
			rest = addr
			doubles = gsub(/::/, "", rest)
			if (doubles > 1) exit 1
			n = split(addr, part, ":")
			groups = 0
			for (i = 1; i <= n; i++) {
				if (length(part[i]) > 4) exit 1
				if (part[i] != "") groups++
			}
			if (doubles ? groups > 7 : groups != 8) exit 1
		}'
}

# cpxy_instance <name>: loads one instance's settings into CPXY_* variables.
cpxy_instance() {
	local i name="$1" domain
	config_get CPXY_INDEX "$name" index
	i="${CPXY_INDEX:-0}"
	_cpxy_uint "$i" 0 9 || i=0

	cpxy_get CPXY_SERVER "$name" server
	cpxy_get CPXY_PROXY_ROUTES "$name" proxy_route
	cpxy_get CPXY_NETWORKS "$name" network
	cpxy_get_bool CPXY_CLIENT "$name" client 1
	cpxy_get_bool CPXY_ROUTER "$name" router 1
	cpxy_get_bool CPXY_TSGW "$name" tsgw 1
	cpxy_get CPXY_IPV6 "$name" ipv6 full

	_cpxy_own CPXY_HTTP_LISTEN "$name" client_http_listen "[::]:$((5000 + i))"
	cpxy_get_bool CPXY_SOCKS5 "$name" client_socks5 0
	_cpxy_own CPXY_SOCKS5_LISTEN "$name" client_socks5_listen "[::]:$((1080 + i))"
	CPXY_PROXY="http://$(_cpxy_connect_addr "$CPXY_HTTP_LISTEN")"

	cpxy_get CPXY_DNS_SEARCH_DOMAINS "$name" dns_search_domain
	cpxy_get CPXY_DNS_UPSTREAM "$name" dns_upstream
	# Overseas servers, asked through the instance's cpxy server
	cpxy_get CPXY_DNS_ALTERNATIVE "$name" dns_alternative "tcp://1.1.1.1?via=cpxy"
	cpxy_get_bool CPXY_DNS_CACHE "$name" dns_cache 1
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

	cpxy_get_bool CPXY_ROUTER_API "$name" router_api 0
	_cpxy_own CPXY_ROUTER_API_LISTEN "$name" router_api_listen "127.0.0.1:$((3010 + i))"
	cpxy_get_bool CPXY_ROUTER_DNS "$name" router_dns 1
	_cpxy_own CPXY_TUN "$name" tun "cpxy$i"
	_cpxy_own CPXY_TABLE "$name" routing_table "$((100 + i))"
	_cpxy_own CPXY_PRIORITY "$name" rule_priority "$((9100 + 10 * i))"
	_cpxy_own CPXY_DNS_LISTEN "$name" dns_listen "0.0.0.0:$((5335 + 10 * i))"

	_cpxy_own CPXY_TSGW_HOSTNAME "$name" tsgw_hostname "cpxy-$(echo "$name" | tr 'A-Z_' 'a-z-')"
	# Not tailscaled's 41641: the router may run Tailscale too
	_cpxy_own CPXY_TSGW_PORT "$name" tsgw_port "$((41700 + i))"
	_cpxy_own CPXY_TSGW_STATE_DIR "$name" tsgw_state_dir "/etc/cpxy/tsgw/$name"
	cpxy_get CPXY_TSGW_AUTHKEY "$name" tsgw_authkey
	cpxy_get CPXY_TSGW_CONTROL_URL "$name" tsgw_control_url
	cpxy_get CPXY_TSGW_MTU "$name" tsgw_mtu 1280
	cpxy_get_bool CPXY_TSGW_PROXY "$name" tsgw_proxy 1
	cpxy_get_bool CPXY_TSGW_VERBOSE "$name" tsgw_verbose 0
}

# cpxy_procd_env <name> <VAR=value>...: the procd instance's environment, which restarts it when
# a value changes. procd (OpenWrt 21.02 at least) stores a changed environment without restarting
# the process, but does restart it when a file it watches changes. The file holds the server key:
# only root may read it.
cpxy_procd_env() {
	local file="$CPXY_RUN/$1.env"
	shift
	mkdir -p "$CPXY_RUN"
	(
		umask 077
		printf '%s\n' "$@" >"$file"
	)
	procd_set_param env "$@"
	procd_set_param file "$file"
}

# Whether a DNS server given with ?via=cpxy can go through the cpxy server, which carries TCP only
_cpxy_dns_via_cpxy_ok() {
	case "$1" in tcp://* | tls://* | https://*) ;; *) return 1 ;; esac
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
	for entry in $CPXY_PROXY_ROUTES; do
		_cpxy_ipv4_cidr "$entry" || _cpxy_ipv6_cidr "$entry" || _cpxy_invalid "$1: invalid proxy_route '$entry'"
	done
	case "$CPXY_IPV6" in
	off | direct | full) ;;
	*) _cpxy_invalid "$1: option 'ipv6' must be off, direct or full" ;;
	esac

	if [ "$CPXY_CLIENT$CPXY_ROUTER$CPXY_TSGW" != 000 ] && [ -z "$CPXY_SERVER" ]; then
		_cpxy_invalid "$1: option 'server' is not set (in the instance or in defaults)"
	fi
	if [ "$CPXY_CLIENT" = 1 ]; then
		_cpxy_claim tcp "$(_cpxy_port "$CPXY_HTTP_LISTEN")" "$1 client_http_listen"
		[ "$CPXY_SOCKS5" = 1 ] && _cpxy_claim tcp "$(_cpxy_port "$CPXY_SOCKS5_LISTEN")" "$1 client_socks5_listen"
	fi
	if [ "$CPXY_ROUTER" = 1 ] || [ "$CPXY_TSGW" = 1 ]; then
		[ -n "$CPXY_DNS_UPSTREAM" ] || _cpxy_invalid "$1: option 'dns_upstream' is required"
		for entry in $CPXY_DNS_UPSTREAM $CPXY_DNS_ALTERNATIVE; do
			case "$entry" in
			*via=cpxy*) _cpxy_dns_via_cpxy_ok "$entry" ||
				_cpxy_invalid "$1: DNS server '$entry' goes through the cpxy server, which carries tcp://, tls:// and https:// only" ;;
			esac
		done
	fi
	if [ "$CPXY_ROUTER" = 1 ] && [ "$CPXY_ROUTER_API" = 1 ]; then
		_cpxy_api_address "$CPXY_ROUTER_API_LISTEN" || _cpxy_invalid "$1: router_api_listen must be an IPv4:port or [IPv6]:port address, port 1..65535"
		_cpxy_claim tcp "$(_cpxy_port "$CPXY_ROUTER_API_LISTEN")" "$1 router_api_listen"
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
