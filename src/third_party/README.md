# Shared dependencies

The core, native bridge, CLI, server, and browser app use this repository-level
override.

## mailparse 0.16.1

Vendored from the published crate, retaining its [0BSD license](mailparse/LICENSE).
Ahrara adds bounded MIME parsing: 256 parts, depth 32, 16 KiB parameter headers,
128 parameter slots, and validated multipart boundaries. Boundary and continuation
scans avoid repeatedly traversing the full input. Public helper signatures remain
unchanged.

Exceeding a budget returns a parsing error; Ahrara keeps the original EML.
Run the upstream and local regressions with `make test` or:

```sh
make cargo ARGS='test --locked --manifest-path /work/src/third_party/mailparse/Cargo.toml'
```
