# syntax=docker/dockerfile:1
#
# Inference gateway. The binary is pure Rust, so it is cross-compiled to a
# fully static musl executable on the build machine's own architecture (no
# emulation) and shipped on distroless/static: no shell, no libc, no package
# manager, non-root.

ARG RUST_IMAGE=rust:1.100-slim-trixie@sha256:3b199fd6db7d709274df56565b8f4003f9e30d7bfcff9d53148c86053b7855e2
ARG RUNTIME_IMAGE=gcr.io/distroless/static-debian13:nonroot@sha256:8ce6dd87e9823c5b48dcfb2c8604714ccb351ce59607327bcc4957448da279b5

# --- build ------------------------------------------------------------------
FROM --platform=$BUILDPLATFORM ${RUST_IMAGE} AS build
ARG TARGETARCH
WORKDIR /src

RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-musl ;; \
      arm64) echo aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac > /target && rustup target add "$(cat /target)"

# rustc's bundled linker handles musl on its own; no C cross-toolchain needed.
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld

# Dependencies first, in their own layer, so code changes don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
 && cargo build --release --locked --target "$(cat /target)" \
 && rm -rf src

COPY src src
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --locked --target "$(cat /target)" \
 && cp "target/$(cat /target)/release/gateway" /gateway

# --- runtime ----------------------------------------------------------------
FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.title="gateway" \
      org.opencontainers.image.description="Inference gateway for protein embeddings" \
      org.opencontainers.image.licenses="Apache-2.0"

COPY --from=build /gateway /gateway
# distroless "nonroot" user, numeric so Kubernetes can verify runAsNonRoot.
USER 65532:65532
# 8080: API; 9100: metrics
EXPOSE 8080 9100
ENTRYPOINT ["/gateway"]
