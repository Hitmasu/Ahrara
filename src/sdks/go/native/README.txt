Release packaging places the matching compiled Rust library under
native/<goos>-<goarch>/ (for example native/linux-amd64/libahrara_native.so).
Go embeds staged artifacts into the application. Generated native binaries are
not tracked in source control; run make sdk-go-package for Docker packaging.
