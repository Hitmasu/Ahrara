# The Ahrara CLI image, from the static release binary of each Linux architecture
# (make docker-cli stages them as ahrara-amd64 and ahrara-arm64).
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
LABEL org.opencontainers.image.source=https://github.com/Hitmasu/Ahrara \
      org.opencontainers.image.description="Disposable email with a local inbox for AI agents, QA and automation" \
      org.opencontainers.image.licenses=MIT
ARG TARGETARCH
COPY --chmod=755 ahrara-${TARGETARCH} /usr/local/bin/ahrara
# ahrara update then points at docker pull instead of replacing the binary.
COPY <<EOF /usr/local/bin/ahrara.install.json
{"schema_version":1,"method":"docker"}
EOF
# The Identity, inbox and settings live under /home/nonroot; mount a volume there to keep them.
ENTRYPOINT ["/usr/local/bin/ahrara"]
