# syntax=docker/dockerfile:1

# Run Cargo and rustc on the BuildKit host instead of emulating the target CPU.
FROM --platform=$BUILDPLATFORM ghcr.io/rust-cross/cargo-zigbuild:0.23.3 AS builder

ARG TARGETARCH
WORKDIR /usr/src/app
COPY . .

RUN --mount=type=cache,id=cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,id=server-target-${TARGETARCH},target=/usr/src/app/target \
    case "$TARGETARCH" in \
        amd64) rust_target=x86_64-unknown-linux-musl ;; \
        arm64) rust_target=aarch64-unknown-linux-musl ;; \
        *) echo "unsupported target architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac && \
    rustup target add "$rust_target" && \
    cargo zigbuild --release --locked --target "$rust_target" -p server && \
    mkdir -p /out && \
    cp "target/$rust_target/release/server" /out/server

FROM debian:bookworm-slim
COPY --from=builder /out/server /usr/local/bin/server

ENV KEY=
ENV BIND_ADDRESS=0.0.0.0:3000
EXPOSE 3000

CMD ["server"]
