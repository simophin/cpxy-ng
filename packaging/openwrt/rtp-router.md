# HTTP-controlled RTP router

`cpxy-rtp` is a separate, opt-in OpenWrt package. It replaces the former RTP VPN
transport, while the original `cpxy` package and `cpxy-router` service remain
available for comparison. Use OpenWrt with firewall4, `ip-full` and `kmod-tun`.
The first version supports one RTP VPN instance, selecting one or more complete
firewall zones through their network names. It carries IPv4 TCP and UDP.

Build the static `cpxy-rtp` and `cpxy-rtp-router` executables for the router's
architecture, then package them:

```sh
cross build --release --locked --target aarch64-unknown-linux-musl \
  -p rtp-service -p rtp-mobile --bin cpxy-rtp --bin cpxy-rtp-router
packaging/openwrt/build-rtp-ipk.sh aarch64_cortex-a53 0.1.0 \
  target/aarch64-unknown-linux-musl/release out
# OpenWrt APK releases: apk-tools >=3 and openssl, using the router's APK arch.
packaging/openwrt/build-rtp-apk.sh aarch64_cortex-a53 0.1.0-r1 \
  target/aarch64-unknown-linux-musl/release out
```

Installing enables the boot service but leaves the VPN disabled in UCI. On the
endpoint host, set a shared `CPXY_RTP_PASSWORD` of at least 12 characters and run:

```sh
cpxy-rtp endpoint --listen 0.0.0.0:4000 --internet-exit
```

Provision the endpoint's IPv4 forwarding, return routing or NAT for its tunnel
pool as described in [the service guide](../../rtp-service/README.md). Admit TCP
4000 and its allocated UDP sockets; no UDP range is required. The example listens
on all IPv4 interfaces; use an address reachable from the router.

On OpenWrt, edit `/etc/config/cpxy_rtp` (mode 0600). The minimum settings to change
are `entry`, `password` and `enabled`; `network` defaults to `lan`. `entry` is the
endpoint's HTTP control `host:port`. Configure a different network, such as
`guest`, to compare with the retained router on LAN. Every network in the selected
firewall zone must be selected. Disable software/hardware flow offloading.

```uci
config vpn 'main'
    option enabled '1'
    option entry 'vpn.example.net:4000'
    option password 'replace-with-your-shared-password'
    list network 'guest'
    list dns_upstream '223.5.5.5'
    list dns_alternative '8.8.8.8'
    option dns_local_server '127.0.0.1:53'
    list dns_local_domain 'lan'
```

```sh
/etc/init.d/cpxy-rtp-router start
logread -e cpxy-rtp
```

Hostnames start with the system resolver's first address family, retaining its
order within each family and interleaving families. Startup staggers candidates
by 250 ms, with at most two in flight, and selects the first to complete authenticated
HTTP setup and bidirectional UDP path validation. IPv4 has no special preference.
Only the selected path is activated; losing UDP allocations are released. Explicit
IP literals select that family. Resolution/racing happens at startup; repair and
rotation keep using the selected address until the worker restarts. It does not
measure bulk throughput or guarantee the fastest sustained path.

The server assigns the inner IPv4 address. The router stores its persistent UUID
under `/etc/cpxy-rtp/state/main`; keep this directory across restarts. Its TUN is
`rtpr0`, the endpoint's default is `rtp0`. Neither an inner address nor an outbound
interface is required. Only packets arriving from the selected LAN devices enter
policy routing; the router's control, transport and direct DNS sockets use its
normal system routes.

Regional and private destinations use the WAN directly. Other destinations use
RTP. Add `list proxy_route 'CIDR'` to force an IPv4 prefix through RTP, including
prefixes within the regional set. DNS uses the same regional classifier: upstream
resolvers are contacted directly and alternatives through RTP. LAN TCP and UDP
DNS on port 53 is redirected to the split resolver on 5353. Local names go to
local dnsmasq; `dns_local_domain` and `dns_search_domain` are repeatable. Alternative
resolvers currently accept IPv4 addresses and use UDP. IPv6 DNS to the router is refused. External IPv6 is refused
for selected networks; local on-link IPv6 and the router itself remain reachable.
QUIC UDP/443 and TCP/853 are refused, matching the mobile VPN policy. The 1200-byte
inner MTU cannot carry QUIC's required minimum datagram size without fragmentation.

An early boot service restores guards before netifd starts; the packet worker
starts after network/firewall initialization. Policy uses tables 20000/30000, priorities 12000–12004, the nft table
`inet cpxy_rtp_guard`, and firewall UCI sections prefixed `cpxy_rtp_`. These resources
must be free at first activation. The service rejects overlap with enabled
`cpxy-router` networks/devices. Keep the services on different network devices;
do not subsequently enable the old router on devices assigned to RTP.

`reload` validates before replacing the worker. Gateway and TUN changes are
reconciled by its supervisor. Independent routing and nft guards protect against
fallback during staging, process failure, netifd changes, firewall reload and
boot. Regional routes remain direct when the RTP worker fails. `stop` deliberately
retains protection; it does not restore unrestricted WAN routing. To release the
selected networks, run:

```sh
/etc/init.d/cpxy-rtp-router cleanup
```

Cleanup removes only the RTP service's owned routing/firewall resources and keeps
its UUID. Run cleanup before changing the selected devices. Package removal runs
cleanup. Changing `enabled` to 0 prevents the worker from starting; previously
installed guards remain until explicit cleanup. The old service/configuration is
never removed by installing or cleaning up RTP.

## Optional regional IPv6 forwarding

Set `option ipv6_direct '1'` in `cpxy_rtp.main` to allow China IPv6 destinations
through ordinary kernel routing. The reviewed `CN6` dataset supplies both the
firewall's exact prefix set and the DNS classifier. RTP and its TUN remain
IPv4-only. DNS returns only regional AAAA answers, independently of a name's IPv4
classification and without an additional A lookup. Foreign AAAA answers become
NODATA; existing local/alternative resolver selection is retained.

The independent guard permits only regional IPv6 destinations, resets blocked
TCP connections and sends ICMPv6 administratively prohibited for other blocked
IPv6 traffic. IPv6 DNS remains refused so clients use the IPv4 split resolver.
Router-originated IPv6 (including RTP control/UDP) is outside this selected-LAN
policy. Local router access and on-link neighbor discovery remain available.
QUIC over regional native IPv6 is permitted; the RTP IPv4 QUIC refusal is unchanged.

Provision IPv6 on the selected network through netifd and odhcpd separately. For
example, an existing delegated `/64` can have RA/DHCPv6 re-enabled after installing
the guard; DNS advertisements should continue pointing clients at the IPv4 split
resolver. The package does not edit network/DHCP settings. Persisted IPv6 prefixes
restore protection before netifd starts, independently of packet-worker health.
Turning the option off restores the previous IPv6 terminal guard. Applications
must implement IPv4 fallback themselves; resets/errors cannot force it, and IPv6-only
nonregional services remain unavailable. ICMP rate limiting can delay repeated
error notifications under load.

The namespace fixture tests native regional TCP/UDP, fast nonregional rejection,
router-originated traffic, local ICMPv6, guard reinstatement and option disablement:

```sh
unshare -Urnm bash packaging/openwrt/test/ipv6-direct.sh
```

## Disposable QEMU validation

The fixture uses an official pinned OpenWrt 24.10.0 x86-64 image, an ephemeral QEMU
snapshot and private network/mount namespaces. It creates two LAN segments, WAN,
HTTP/UDP/DNS destinations and a separate RTP endpoint. No live router or public
server is modified. Firmware and dependency SHA-256 values are checked by the
fetcher against the pinned image and official release feed manifests.

Install host `qemu-system-x86`, `iproute2`, `curl`, Python 3 and sudo.
Build x86-64 static binaries and an IPK as above. Use a private asset directory:

```sh
assets=$(mktemp -d /tmp/cpxy-rtp-qemu-XXXXXX)
python3 packaging/openwrt/test/fetch-rtp-qemu-assets.py "$assets"
packaging/openwrt/build-rtp-ipk.sh x86_64 0.1.0 \
  target/x86_64-unknown-linux-musl/release "$assets"
sudo env RTP_SERVICE_BIN=target/x86_64-unknown-linux-musl/release/cpxy-rtp \
  RTP_QEMU_ASSETS="$assets" unshare -nm \
  bash rtp-service/tests/private-linux-lab.sh -c '
    python3 packaging/openwrt/test/qemu-rtp.py \
      "$RTP_QEMU_ASSETS/openwrt.img" \
      "$RTP_QEMU_ASSETS/cpxy-rtp_0.1.0_x86_64.ipk" \
      "$RTP_QEMU_ASSETS/deps" "$RTP_SERVICE_BIN" "$RTP_QEMU_ASSETS/results"
  '
```

Logs and captures are private fixture artifacts; the serial log contains the
random test password. Delete the asset/results directory when finished. Firmware
emulation validates OpenWrt integration, not the physical router's acceleration,
radio, hardware MTU or throughput. See the repository validation report for actual
results and remaining limitations.

Physical router throughput, MTU checks, receive-buffer behavior and deployment
rollback details are recorded in the [2026-10-28 throughput investigation](../../docs/rtp-router-throughput-results.md).
