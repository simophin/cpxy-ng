#!/bin/sh
# Sourced by lab.sh after the legacy checks; uses that lab's isolated topology and assertions.
GEO="$(dirname "$NET")/geo.sh"
"$BIN/cpxy-router" --export-direct-routes >"$WORK/direct.cidrs"
geo_up() { in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_up '$WORK/direct.cidrs' 192.0.2.1 wan0r lan0r"; }
geo_direct() { in_lan curl -fsS --max-time 3 --noproxy '*' http://114.114.114.114:8000/index.html; }
check "GeoIP full dataset installs" geo_up
check "GeoIP direct TCP works with no worker or server" geo_direct
check "GeoIP regional IPv4 ping works without a worker or server" in_lan ping -c1 -W2 114.114.114.114
check "GeoIP overseas IPv4 ping works without a worker or server" in_lan ping -c1 -W2 93.184.216.34
check_not "GeoIP regional IPv6 ping is refused without a worker or server" in_lan ping -6 -c1 -W2 2400:3200::114
check_not "GeoIP overseas IPv6 ping is refused without a worker or server" in_lan ping -6 -c1 -W2 2001:db8:100::34
mark; check_not "GeoIP proxy TCP without worker fails closed" fetch
expect_not_direct "GeoIP no worker does not leak"
check_not "GeoIP overseas IPv6 TCP without worker fails closed" fetch "$URL_V6"
got="$(udp_probe 9999 2001:db8:100::34)"
[ "$got" = refused ] && pass "GeoIP refuses ordinary IPv6 UDP" || fail "GeoIP IPv6 UDP failed: $got"
got="$(udp_probe 443)"
[ "$got" = refused ] && pass "GeoIP refuses QUIC" || fail "GeoIP QUIC not refused: $got"
got="$(udp_probe 9999)"
[ "$got" = 'reply from 192.0.2.2' ] && pass "GeoIP preserves ordinary IPv4 UDP" || fail "GeoIP IPv4 UDP changed: $got"

bg ip netns exec inet "$BIN/cpxy-server" --key lab '[2001:db8:2::1]:8443' >>"$WORK/server.log" 2>&1
SERVER_PID=$!
bg ip netns exec router "$BIN/cpxy-router" --socks5-proxy 127.0.0.1:1080 --tun cpxy0 >>"$WORK/tun.log" 2>&1
TUN_PID=$!
sleep 1
mark; check "GeoIP non-regional TCP is proxied" fetch
expect_seen "GeoIP proxy uses server source" 93.184.216.34
check "GeoIP second instance installs" in_router sh -c ". '$NET'; . '$GEO'; cpxy_net_select cpxy1 101 9110; cpxy_geo_up '$WORK/direct.cidrs' 192.0.2.1 wan0r guest0r"
bg ip netns exec router "$BIN/cpxy-router" --socks5-proxy 127.0.0.1:1081 --tun cpxy1 >>"$WORK/guest-tun.log" 2>&1
GUEST_PID=$!
sleep 1
mark; check "GeoIP second instance uses its assigned exit" guest_fetch
expect_seen "GeoIP second instance source" 192.0.3.1
check "GeoIP repeat generation switches safely" geo_up
check "GeoIP event reload accepts closed stdin" in_router sh -c ". '$NET'; . '$GEO'; exec 0<&-; cpxy_geo_up '$WORK/direct.cidrs' 192.0.2.1 wan0r lan0r"
check "GeoIP active generation is healthy" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_healthy '$WORK/direct.cidrs' wan0r lan0r"
in_router ip -4 rule del priority 9102 iif lan0r ipproto 1 lookup main
check_not "GeoIP health detects a missing ICMP bypass" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_healthy '$WORK/direct.cidrs' wan0r lan0r"
check "GeoIP reload restores the ICMP bypass" geo_up
check "GeoIP restored IPv4 ping works" in_lan ping -c1 -W2 93.184.216.34

# Simulate a deployed dual-family generation. Reload must retire both old IPv6
# tables and every forwarding selector while preserving terminal guards.
in_router ip -6 route add default dev cpxy0 table 100
in_router ip -6 route add default dev cpxy0 table 10100
in_router ip -6 rule add priority 9103 iif lan0r ipproto tcp lookup 100
in_router ip -6 rule add priority 9102 iif lan0r ipproto udp lookup main
check_not "GeoIP health detects obsolete IPv6 state" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_healthy '$WORK/direct.cidrs' wan0r lan0r"
check "GeoIP upgrades old dual-family state" geo_up
check "GeoIP upgraded state is healthy" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_healthy '$WORK/direct.cidrs' wan0r lan0r"
check "GeoIP upgrade clears both IPv6 tables" in_router sh -c '[ -z "$(ip -6 route show table 100 2>/dev/null)$(ip -6 route show table 10100 2>/dev/null)" ]'
check_not "GeoIP upgraded IPv6 stays refused" fetch "$URL_V6"

# Force a whole /8 containing the more-specific regional test destination through the proxy.
"$BIN/cpxy-router" --export-direct-routes --proxy-route 114.0.0.0/8 >"$WORK/direct.cidrs"
check "GeoIP broad forced-proxy update installs" geo_up
lines="$(wc -l <"$WORK/direct.log")"
check "GeoIP formerly regional address remains reachable" geo_direct
tail -n "+$((lines + 1))" "$WORK/direct.log" | grep -q '^114.114.114.114 ' &&
	pass "GeoIP forced regional address uses server source" || fail "GeoIP forced regional address went direct"

# A batch failure must leave the old complete generation selected.
active="$(in_router ip -4 rule show priority 9103)"
printf 'not-a-cidr\n' >"$WORK/bad.cidrs"
check_not "GeoIP malformed batch cannot activate" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_up '$WORK/bad.cidrs' 192.0.2.1 wan0r lan0r"
[ "$active" = "$(in_router ip -4 rule show priority 9103)" ] && pass "GeoIP failed batch retains selector" || fail "GeoIP failed batch changed selector"
mark; check "GeoIP old generation still proxies after failure" fetch
expect_not_direct "GeoIP failed batch has no WAN fallback"

"$BIN/cpxy-router" --export-direct-routes --proxy-route 0.0.0.0/0 >"$WORK/direct.cidrs"
check "GeoIP all-proxy policy accepts an empty direct list" geo_up
check "GeoIP empty direct list still reaches regional destination via server" geo_direct
check "GeoIP all-proxy policy keeps IPv4 ping direct" in_lan ping -c1 -W2 114.114.114.114
"$BIN/cpxy-router" --export-direct-routes >"$WORK/direct.cidrs"
check "GeoIP restores automatic regional policy" geo_up
check "GeoIP unavailable WAN stages direct refusals" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_up '$WORK/direct.cidrs' - - lan0r"
check_not "GeoIP missing WAN does not move regional TCP into proxy" geo_direct
check "GeoIP WAN recovery restores direct routes" geo_up
check "GeoIP recovered WAN direct TCP works" geo_direct
active="$(in_router ip -4 rule show priority 9103 | awk '/lookup/ { print $NF; exit }')"
# Remove every device-bound direct route, as Linux does during WAN interface teardown.
in_router ip -4 route flush table "$active" dev wan0r
check_not "GeoIP removed WAN routes retain per-prefix refusal" geo_direct
check "GeoIP restores WAN routes after device teardown" geo_up

active="$(in_router ip -4 rule show priority 9103 | awk '/lookup/ { print $NF; exit }')"
in_router ip -4 route flush table "$active"
mark; check_not "GeoIP missing table fails closed" fetch
expect_not_direct "GeoIP missing table has no WAN fallback"
mark; check "GeoIP main table failure leaves guest working" guest_fetch
expect_seen "GeoIP guest exit survives main failure" 192.0.3.1
check "GeoIP restores a deleted table" geo_up

in_router ip link del cpxy0
mark; check_not "GeoIP deleted TUN fails closed" fetch
expect_not_direct "GeoIP deleted TUN has no WAN fallback"
kill "$TUN_PID" "$SERVER_PID" "$GUEST_PID"
check "GeoIP guest stop cleans its generations" in_router sh -c ". '$NET'; . '$GEO'; cpxy_net_select cpxy1 101 9110; cpxy_geo_down"
check "GeoIP stop cleans both generations" in_router sh -c ". '$NET'; . '$GEO'; cpxy_geo_down"
check "GeoIP stop clears owned IPv6 table" in_router sh -c '[ "$(ip -6 route show table 100 2>/dev/null | wc -l)" = 0 ]'
mark; check "GeoIP stop restores ordinary WAN" fetch
expect_seen "GeoIP stop uses WAN source" 192.0.2.2
