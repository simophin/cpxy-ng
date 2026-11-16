#!/bin/sh
# Run by the package's install hooks. Packages before local_region had built-in DNS defaults:
# dns_alternative 'tcp://1.1.1.1?via=cpxy', and client_dns_server '127.0.0.1' for cn_split. Those
# now depend on local_region and have no default, so a configuration from before (one without
# local_region) gets them written into its defaults section and keeps working the same.
[ -z "${IPKG_INSTROOT}" ] || exit 0
[ -f /etc/config/cpxy ] || exit 0
uci -q get cpxy.defaults >/dev/null || exit 0
[ -z "$(uci -q get cpxy.defaults.local_region)" ] || exit 0
uci set cpxy.defaults.local_region=cn
[ -n "$(uci -q get cpxy.defaults.dns_alternative)" ] ||
	uci add_list cpxy.defaults.dns_alternative='tcp://1.1.1.1?via=cpxy'
[ -n "$(uci -q get cpxy.defaults.client_dns_server)" ] ||
	uci add_list cpxy.defaults.client_dns_server=127.0.0.1
uci commit cpxy
