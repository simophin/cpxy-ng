Source: https://github.com/simophin/ipstack, revision
49720c1a1effacee1073d419452432629ef64297 (Apache-2.0, LICENSE retained).

This copy adds only `IpStackUdpStream::recv_datagram` / `send_datagram` in
`src/stream/udp.rs`. They distinguish an empty datagram from channel closure,
reset the idle deadline on actual traffic, and reject replies exceeding the TUN
MTU instead of silently truncating or emitting several UDP packets. Existing
stream APIs and TCP behavior remain unchanged. The packet engine uses these new
methods for ordinary direct UDP; DNS keeps its existing framing and limits.

Examples, the Criterion benchmark module in `packet.rs`, and unrelated development
dependencies/package profiles are omitted. Remove the Cargo
patch and vendored source when the pinned upstream revision supplies this API.
