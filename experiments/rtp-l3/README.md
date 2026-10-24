# Experimental RTP-framed IPv4 tunnel

Linux-only TUN runtime; portable protocol tests. This is encrypted IP in an RTP
header, **not codec media, SRTP, or demonstrated censorship resistance**. The
custom PSK control exchange is readily distinguishable. No production integration.

Build/test with `cargo test --locked` and `cargo build --release --locked` in this
directory. The crate is deliberately outside the production Cargo workspace.

Generate an exactly 32-byte random key in a private file outside the checkout,
mode 0600, and transfer it securely. The runtime refuses group/world-readable
keys. Never print or commit keys. Both sides use the same file contents.

Server (authorized Linux account with TUN privileges):

```sh
rtp-l3-poc server KEY_FILE TUN_NAME INNER_SERVER_CIDR LISTEN_ADDRESS:PORT
```

Client (ordinary account supporting unprivileged user/network namespaces):

```sh
python3 scripts/client-netns.py BINARY KEY_FILE SERVER_ADDRESS:PORT TUN_NAME INNER_CLIENT_CIDR RUN_SCRIPT
```

The launcher creates and connects UDP **before** entering `unshare -Urn`. The
socket retains host networking while the TUN and `RUN_SCRIPT` applications live
in an isolated network namespace. `RUN_SCRIPT` can configure a default route via
the TUN if server forwarding is configured. IP addresses, endpoints, firewall
rules and private deployment locations are operator-supplied, never hardcoded.
Server does not alter firewall, forwarding sysctls or NAT automatically. Restrict
its temporary UDP firewall opening to the test source. Use runtime rules only and
explicitly remove them afterwards. Closing TUN FDs removes task interfaces.

The event loop polls nonblocking TUN/UDP with bounded 64-packet batches, no
unbounded user queue, a requested 4 MiB kernel socket buffer, and 1200-byte inner
MTU. It emits JSON counters every five seconds and on SIGTERM/SIGINT. Socket
send/TUN write failures are counted and dropped. Authentication failures and
replays share a counter; iperf's receiver report supplies final UDP loss/jitter.
Counters include UDP payload bytes, excluding outer IP/UDP overhead; IPv4 wire
size is inner length + 56 bytes. UDP setup packets are separate.

See `docs/rtp-l3-poc-handoff.md` at repository root for wire design, checkpoints,
measurement plan and cleanup evidence. A 128-packet replay window tolerates
bounded reordering. Gaps of half a sequence cycle or longer require restarting
both peers. There is no reconnect timer, congestion controller, pacing, FEC,
fragmentation, certificate authentication or forward secrecy. Endpoint TCP retains
its own congestion control. Use bounded paced UDP experiments only.
