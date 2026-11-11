# syntax=docker/dockerfile:1

# Run Cargo and rustc on the BuildKit host instead of emulating the target CPU.
FROM --platform=$BUILDPLATFORM ghcr.io/rust-cross/cargo-zigbuild:0.23.3 AS builder

ARG TARGETARCH
WORKDIR /usr/src/app
COPY . .

RUN --mount=type=cache,id=cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,id=cpxy-target-${TARGETARCH},target=/usr/src/app/target \
    case "$TARGETARCH" in \
        amd64) rust_target=x86_64-unknown-linux-musl ;; \
        arm64) rust_target=aarch64-unknown-linux-musl ;; \
        *) echo "unsupported target architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac && \
    rustup target add "$rust_target" && \
    cargo zigbuild --release --locked --target "$rust_target" -p cpxy --bin cpxy && \
    mkdir -p /out && \
    cp "target/$rust_target/release/cpxy" /out/cpxy

# A static musl binary: TLS roots are built in, and the container runtime provides
# /etc/resolv.conf and /etc/hosts.
FROM scratch
COPY --from=builder /out/cpxy /cpxy
USER 65534:65534

# `cpxy server` by default; run another subcommand by giving it, e.g. `docker run <image> client ...`.
# The pre-shared key is required: pass it at run time (-e KEY=... or a secret), never in the image.
ENV BIND_ADDR=0.0.0.0:3000
EXPOSE 3000

ENTRYPOINT ["/cpxy"]
CMD ["server"]
