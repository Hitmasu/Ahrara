FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools=1.2.3-1 && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
RUN rustup component add rustfmt clippy && \
    case "$TARGETARCH" in amd64) rustup target add x86_64-unknown-linux-musl ;; arm64) rustup target add aarch64-unknown-linux-musl ;; *) exit 1 ;; esac
WORKDIR /work/cli
