# Client dependencies

## mailparse 0.16.1

Vendored from the published crate, retaining its [0BSD license](mailparse/LICENSE).
Ahrara adds bounded MIME parsing: 256 parts, depth 32, 16 KiB parameter headers,
128 parameter slots, and validated multipart boundaries. Boundary and continuation
scans avoid repeatedly traversing the full input. Public helper signatures remain
unchanged.

Exceeding a budget returns a parsing error; Ahrara keeps the original EML.
Run the upstream and local regressions with `make test` or:

```sh
make cargo ARGS='test --locked --manifest-path third_party/mailparse/Cargo.toml'
```

## libsqlite3-sys 0.35.0 / SQLCipher 4.19.0

The base crate retains its [MIT license](libsqlite3-sys/LICENSE). Its SQLCipher
bundle is replaced by Community 4.19.0 (SQLite 3.53.4), retaining the
[SQLCipher license](libsqlite3-sys/sqlcipher/LICENSE-4.19.md). Only
`sqlcipher/sqlite3.c`, `sqlcipher/sqlite3.h`, and `build.rs` differ from the base
crate. The build supplies the initialization/shutdown macros for newer SQLCipher;
the Rust API and pregenerated bindings are unchanged.

| Source | Provenance |
|---|---|
| [libsqlite3-sys 0.35.0](https://crates.io/crates/libsqlite3-sys/0.35.0) | Archive SHA-256 `133c182a6a2c87864fe97778797e46c7e999672690dc9fa3ee8e241aa4a9c13f` |
| [SQLCipher source](https://github.com/sqlcipher/sqlcipher/tree/c4b275a47932888216bade83aff2bbc73df0ff85) | Commit `c4b275a47932888216bade83aff2bbc73df0ff85` |
| SQLCipher source archive | SHA-256 `e56d1f43cb18ec90d68b4b52d54c00b8b1c11605a27fbbba4249a1d6fc042a6f` |
| Generated `sqlcipher/sqlite3.c` | SHA-256 `8640c653acadf665cce6331646f60b5b74a4690746f2c4a2d8f688a0570a0c0c` |
| Generated `sqlcipher/sqlite3.h` | SHA-256 `8a9d1bff44d75174ca6dea3ea9bac50a6104d86facb566647b8bb839375b7b3a` |

Generate the amalgamation in a separate build directory with `../source/configure`
and `make -j1 sqlite3.c`; copy only `sqlite3.c` and `sqlite3.h`. SQLCipher 4.19 keeps
the SQLCipher 4 database format. Its backup API rejects encrypted databases;
conversion uses `sqlcipher_export`.

Keep these overrides until upstream provides equivalent fixes. Run `make test`
after updates, including the storage/migration checks and engine-version assertions.
