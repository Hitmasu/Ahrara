FROM ghcr.io/rust-cross/cargo-zigbuild:0.23.4@sha256:d8313491ec5798de0633fdc1c5753761bff79967bea69076020dc78121b2cca8 AS cross-tools

FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS native
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools=1.2.3-1 && rm -rf /var/lib/apt/lists/*
ARG TARGETARCH
RUN rustup component add rustfmt clippy && \
    case "$TARGETARCH" in amd64) rustup target add x86_64-unknown-linux-musl ;; arm64) rustup target add aarch64-unknown-linux-musl ;; *) exit 1 ;; esac
WORKDIR /work/src/cli

FROM native AS cross
COPY --from=cross-tools /usr/local/zig-x86_64-linux-0.16.0 /opt/zig
COPY --from=cross-tools /opt/MacOSX11.3.sdk /opt/MacOSX11.3.sdk
ENV PATH="/opt/zig:${PATH}" SDKROOT=/opt/MacOSX11.3.sdk MACOSX_DEPLOYMENT_TARGET=11.0
RUN rustup target add aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin \
    x86_64-pc-windows-gnu aarch64-pc-windows-gnullvm

# openssl-src does not map Windows ARM64 yet. Build the same locked source directly.
ADD --checksum=sha256:46eb8fb9fb3b61ce1c0f8a026c4c1a0714d3a9e138e7fbde78753ce2babc3846 \
    https://static.crates.io/crates/openssl-src/openssl-src-300.6.1%2B3.6.3.crate /tmp/openssl.crate
RUN mkdir /tmp/openssl && tar -xf /tmp/openssl.crate -C /tmp/openssl --strip-components=1 && \
    cd /tmp/openssl/openssl && \
    CC="zig cc -target aarch64-windows-gnu" AR="zig ar" RANLIB="zig ranlib" \
    perl ./Configure mingwarm64 no-shared no-module no-tests no-apps no-asm \
        --prefix=/opt/openssl-aarch64 --libdir=lib && \
    make -j2 build_libs && make install_dev && rm -rf /tmp/openssl /tmp/openssl.crate /root/.cache
ENV AARCH64_PC_WINDOWS_GNULLVM_OPENSSL_NO_VENDOR=1 \
    AARCH64_PC_WINDOWS_GNULLVM_OPENSSL_STATIC=1 \
    AARCH64_PC_WINDOWS_GNULLVM_OPENSSL_DIR=/opt/openssl-aarch64
ADD --checksum=sha256:9e3cf73485edbd45905c8aadbc0fdf869c7ddc3848f0c898229f2680db52e44b \
    https://github.com/rust-cross/cargo-zigbuild/releases/download/v0.23.4/cargo-zigbuild-x86_64-unknown-linux-musl.tar.xz /tmp/zigbuild.tar.xz
RUN tar -xJf /tmp/zigbuild.tar.xz -C /usr/local/cargo/bin --strip-components=1 \
    cargo-zigbuild-x86_64-unknown-linux-musl/cargo-zigbuild && rm /tmp/zigbuild.tar.xz
