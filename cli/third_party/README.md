# mailparse 0.16.1

Source: the `mailparse` 0.16.1 crate, 0BSD license retained in `mailparse/LICENSE`.
The functional change adds a shared 256-part budget and maximum nesting depth 32
to `parse_mail_recursive`. MIME parsing is optional: Ahrara retains the original
EML and reports `parse_error` when this budget is exceeded. There is no custom
MIME parser. A unit test checks exhausted budgets; an allow attribute preserves
upstream lifetime signatures under newer Rust. Upstream tests remain available with:

```sh
./scripts/cargo.sh test --manifest-path third_party/mailparse/Cargo.toml
```

Keep this patch until the upstream parser provides equivalent bounded parsing.
