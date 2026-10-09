# Multi-instance OpenWrt validation

Validated on 2026-10-09 against main `f9e77e8`, on a GL.iNet GL-MT3000 running
OpenWrt 21.02-SNAPSHOT `r15812+926-46b6ee7ffc`, Linux 5.4.211 and fw3/iptables.
The final test package is `cpxy-router` `0.0.0-multi-test3`. The native Rust binaries
are unchanged from main's previously validated ARM64 build; these changes concern
OpenWrt supervision, routing, firewall configuration and packaging.

The old installed cpxy package was stopped, disabled and removed. Its configuration,
staged UCI state, legacy binaries, DNS drop-in, fixed firewall sections, TUN and
routing state were purged before installation. A fresh configuration reused only the
existing main endpoint credentials and explicitly selected the DNS settings; guest
was configured with the second endpoint provided by the user. No keys are recorded here.

## Physical setup

The test host connects via `wlan0`; wired Ethernet remains the preferred internet
route via `192.168.0.1`. HTTPS probes bind to `wlan0`; DNS and port probes use
`SO_BINDTODEVICE`. Wi-Fi switching uses `sudo iwctl`, with an explicit disconnect
before connecting to the other SSID. Tailscale provides stable router SSH management
while switching networks, using the already trusted router SSH host key.

| Network | SSID | Router address | Worker resources | Upstream / observed exit |
|---|---|---|---|---|
| main | `FF-TECH-5G` | `192.168.7.1` | `cpxy0`, table 100, priorities 9100–9103 | Sydney / `85.155.188.78` (AU) |
| guest | `GL-MT3000-991-5G-Guest` | `192.168.9.1` | `cpxy1`, table 101, priorities 9110–9113 | Hong Kong / `216.250.97.252` (HK) |

The wired direct baseline was `49.190.33.83`. Main owns one shared DNS worker on
`127.0.0.1:5335`, with its RAM-backed SQLite cache. Existing guest firewall policy
was retained; no new isolation feature was introduced. Temporary guest SSH access
used during initial tests was removed. The guest radio was enabled using main's
existing Wi-Fi passphrase, and the host was restored to main Wi-Fi after testing.

## Results

| Check | Result |
|---|---|
| HTTPS on both physical SSIDs | Different assigned upstream exits, distinct from the direct baseline |
| DNS to the router, UDP and TCP, both networks | Successful A answers |
| QUIC and TCP 853, both networks | Immediate refusals, approximately 1–6 ms |
| Firewall reload | Guest upstream retained |
| Unchanged service reload | All three worker PIDs retained; existing routes kept; no pending firewall edits |
| Guest worker suspended/resumed | New guest HTTPS timed out while suspended; resumed proxy restored |
| Invalid guest priority collision | Reload rejected; workers and routing retained |
| Guest disabled/re-enabled | Direct WAN restored while disabled; second upstream restored when enabled; main and DNS PIDs unchanged |
| Guest TUN/table/priorities reassigned and restored | Old resources cleaned; new upstream path worked; main and DNS PIDs unchanged |
| Guest worker killed | procd respawned guest; main and DNS PIDs unchanged |
| Host switched back to main | Original main upstream restored; wired default route retained |
| Router reboot | Both packet workers and shared DNS started automatically; both SSIDs retained their assigned exits and working DNS |

After the first reboot, the radio's automatic channel changed from 44 to 36 and
the host's iwd scan state stopped listing the main SSID even though raw scans
showed it broadcasting. The test radio was pinned to its previously working
channel 44 and the host's iwd service restarted. Main and guest exits and DNS
were then verified, without restarting cpxy on the router. A second full router
reboot with channel 44 confirmed both SSIDs and their assigned exits/DNS again,
with both packet workers and DNS started automatically. Only the host Wi-Fi
service was refreshed for discovery; no router Wi-Fi or cpxy restart was needed.

The resource-change check caught and fixed a procd API interaction: calling
`procd_kill` inside an open service update overwrites its JSON message. Worker
replacement/removal now happens when procd publishes the complete service update.
The init regression check rejects calls that would reintroduce this interaction.

## Automated checks

The two-upstream namespace routing lab passed, including different upstream source
addresses, guest reload, fail-closed guest worker failure, guest stop and unaffected
main traffic. The OpenWrt 24.10 container checks passed for real UCI zone discovery
(including scalar and list network membership), firewall generation, resource collision
validation, instance disable/deletion/reassignment, package install/removal and shared
DNS readiness/exit handover. ShellCheck passed with the same warning settings as CI.

Hardware validation uses fw3. A physical fw4 router, sustained throughput and WAN
reconnection are not covered by these results. Existing regional-direct routing,
unsupported UDP behavior and IPv6 policy remain those of the native engine/package.
