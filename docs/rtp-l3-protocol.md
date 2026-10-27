# RTP transport protocol

The Linux service now implements [HTTP-controlled transport v2](rtp-http-protocol.md).
It uses encrypted JSON-RPC over plain HTTP for allocation, dedicated random UDP
ports, authenticated/replay-checked RTP-shaped frames, and acknowledged
client-driven handover. There is no base UDP listener or v1 negotiation.

The [v1 protocol](archive/rtp-l3-protocol-v1.md) is retained as historical context
for earlier experiment reports. Its unauthenticated data format has been removed.
See the [implementation plan](rtp-http-control-plan.md),
[validation report](rtp-http-validation.md), and
[Linux service instructions](../rtp-service/README.md).
