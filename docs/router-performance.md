# Router latency

## Changes from the code review

The server used to wait up to 500 ms for destination data before confirming a
tunnel. The router opens tunnels with empty `initial_plaintext` and only starts
relaying after confirmation. For client-first protocols, including HTTPS, the
destination waits for the client's request, so this added the full timeout to
each new proxied connection.

The server now confirms as soon as the destination connection and any requested
TLS handshake and initial write complete. Greetings and responses to initial
payloads travel through the normal relay. This uses the existing protocol with
an empty `initial_response`; clients already handle that case. **Deploy the
updated cpxy server to get this improvement; updating only the router does not
remove the wait.**

Direct TCP sockets now enable `TCP_NODELAY`, matching the proxy transport and
server's destination sockets. This avoids Nagle buffering for small writes on
direct connections; its benefit depends on the traffic and acknowledgement
timing.

The integration tests cover client-first echo, initial payload responses,
delayed server greetings, and routing through SOCKS5. A 300 ms local tunnel
setup deadline fails against the original server's 500 ms wait and passes with
the change. This is a regression check, not a deployed-router benchmark.

## Next measurements and candidates

1. **Proxy connection setup.** `ProtocolOutbound::send` creates a fresh TCP
   connection and, for HTTPS proxy URLs, performs TLS before each tunnel
   request. A small pool of preconnected transports could move those handshakes
   off the request path. Measure cold and resumed TLS first. Account for idle
   closure by the server or reverse proxy, bounded pool size, and refill costs
   before implementing it. A tunnel cannot be reused for a different destination
   under the current protocol.
2. **DNS tails.** Cache misses race the configured resolver groups. A nonlocal
   upstream answer waits for the alternative group; when that group stalls,
   fallback can take the five-second query timeout. Measure cache hits, misses,
   and failed alternatives separately before changing this routing policy.
   DNS connections are already reused.
3. **Latency under load and loss.** Measure p50/p95/p99 while idle and while
   uploading/downloading. The pinned userspace TCP stack has no congestion
   control or RTT-based retransmission estimator and starts its retransmission
   timer at one second. Packet loss and queueing deserve investigation if the
   long tail remains after fixing setup. Larger windows alone can worsen it;
   the existing window comments document this tradeoff.

For a useful router benchmark, compare direct and proxied TCP, warm and cold DNS,
and warm and cold proxy TLS. Record connection setup and application first-byte
times separately, along with CPU usage and packet drops during concurrent bulk
traffic. The traffic API's `delay_millis` measures outbound connection setup;
it excludes DNS before the connection and does not measure application first-byte
time. SNI reporting waits independently and does not delay forwarding.

Run `cargo test --locked -p server -p client` for the transport checks, and
`cargo test --locked -p mobile-engine` plus the OpenWrt routing lab on Linux for
the packet path. The macOS run cannot validate Linux TUN behavior.
