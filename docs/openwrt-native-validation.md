# Native OpenWrt client: hardware validation

Validated on 2026-10-09 using the `openwrt-native-vpn` implementation based on main
`b395226`. The device is a GL.iNet GL-MT3000, `aarch64_cortex-a53`, running
OpenWrt 21.02-SNAPSHOT (`r15812+926-46b6ee7ffc`), Linux 5.4.211 and firewall3.

The previous `cpxy-router` 1.10.2-test3 package was stopped, disabled and removed.
Its preserved UCI configuration, staged UCI state, backup archive, binaries,
helper scripts, dnsmasq drop-in, owned firewall sections, TUN and policy routes
were purged before installation. Existing unrelated router networking was retained.
Only the endpoint credentials and LAN/DNS values were reused in a fresh configuration.

The new package was installed as `cpxy-router` 0.0.0-native-test with static ARM64
binaries built using the workspace lockfile. The router binary's SHA-256 was
`60d1f4091c550cee6e59ebf2294eb9d405d31a9d75ab9a9a7c949581f71a0814`, matching
the locally built binary. The old SOCKS and statistics listeners were absent.

## Configuration exercised

- Protected network: `lan`, device `br-lan`; TUN `cpxy0`, routing table 100.
- Existing authenticated cpxy endpoint, supplied through `SERVER`.
- DNS splitting enabled on `127.0.0.1:5335`, with the RAM-backed SQLite cache.
- Upstream DNS: `119.119.119.119`; alternatives: `tls://1.1.1.1` and `tls://8.8.8.8`.
- Existing Tailscale configuration remained on the router.

## Results

All checks below passed using traffic from a real Wi-Fi LAN client, with sockets
explicitly bound to `wlan0`. The test computer also had wired Ethernet; initial
port-refusal probes bound only to the Wi-Fi source address took the wired route
and timed out. Corrected probes used `SO_BINDTODEVICE` and received immediate refusals.

| Check | Observed result |
|---|---|
| LAN HTTPS to `1.1.1.1` | Remote exit IP, different from the direct WAN baseline |
| Router-originated HTTPS | Original WAN exit IP |
| DNS through router, UDP and TCP | Successful A replies for `example.com` |
| AAAA query | NOERROR with no address records |
| UDP 443 and TCP 853 | Connection refused in approximately 1 ms |
| Regional TCP to `119.29.29.29:53` | Valid DNS reply; a temporary router OUTPUT counter proved a direct connection |
| Worker suspended with SIGSTOP | LAN HTTPS timed out; persistent TUN remained present |
| Worker resumed with SIGCONT | Remote exit restored |
| Worker killed with SIGKILL | procd started a new worker; remote exit restored |
| Firewall reload | Remote exit preserved |
| Service reload | Remote exit preserved |
| Explicit service stop | Original direct WAN exit restored; owned routes and DNS drop-in removed |
| Service start | Remote exit and DNS handover restored |
| Four concurrent 1,000,000-byte downloads | All returned HTTP 200 and the requested byte count |

After the parallel downloads the packet worker used 4,544 KiB RSS, with a measured
peak of 5,300 KiB and seven threads. These small transfers verify functionality;
they are not a maximum-throughput benchmark. Temporary packet-counter rules were
removed. The new service was left enabled and running.

## Other validation and limits

Both ARM64 and x86_64 musl router builds succeeded. Workspace tests, the mobile
and router namespace labs, OpenWrt package install/removal, init dry run, DNS
handover tests, ShellCheck and actionlint passed. The mobile lab additionally
verified sixteen concurrent 4 MB transfers after the shared-engine refactor.

Hardware checks did not cover reboot, WAN reconnection, fw4 on a physical router,
IPv6 from a globally addressed LAN client, sustained throughput, or every
third-party VPN/offload configuration. No general UDP tunneling or forced DNS
redirection is introduced by this change.
