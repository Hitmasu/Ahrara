FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools=1.2.3-1 && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
ARG AHRARA_SERVER_FEATURES=""
WORKDIR /src
COPY server server
COPY cli/src/protocol.rs cli/src/protocol.rs
COPY cli/src/secure.rs cli/src/secure.rs
COPY cli/third_party/mailparse cli/third_party/mailparse
RUN case "$TARGETARCH" in amd64) target=x86_64-unknown-linux-musl ;; arm64) target=aarch64-unknown-linux-musl ;; *) exit 1 ;; esac && \
    rustup target add "$target" && \
    cargo build --locked --release --features "$AHRARA_SERVER_FEATURES" --target "$target" --manifest-path server/Cargo.toml && \
    cp "server/target/$target/release/ahrara-server" /ahrara-server

FROM scratch
COPY --from=build /ahrara-server /ahrara-server
COPY server/NOTICE /NOTICE
COPY server/third_party/libcfd/LICENSE /licenses/libcfd/LICENSE
USER 65532:65532
EXPOSE 2525 8443
ENTRYPOINT ["/ahrara-server"]
