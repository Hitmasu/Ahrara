FROM golang:1.27.1-bookworm@sha256:69a7b9788769bec032d238959b61854e9ae87f57be9029ec04e9885fabf99195 AS build
WORKDIR /src
COPY . .
RUN CGO_ENABLED=0 go build -mod=readonly -trimpath -ldflags="-s -w" -o /ahrara-server ./cmd/ahrara-server

FROM scratch
COPY --from=build /ahrara-server /ahrara-server
USER 65532:65532
EXPOSE 2525 8443
ENTRYPOINT ["/ahrara-server"]
