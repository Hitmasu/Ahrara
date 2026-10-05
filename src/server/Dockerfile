# Supply the prebuilt static binary with --build-context ahrara-binary=./dist.
FROM scratch
COPY --from=ahrara-binary --chmod=0555 /ahrara-server /ahrara-server
COPY src/server/NOTICE /NOTICE
COPY src/server/third_party/libcfd/LICENSE /licenses/libcfd/LICENSE
USER 65532:65532
EXPOSE 2525 8443
ENTRYPOINT ["/ahrara-server"]
