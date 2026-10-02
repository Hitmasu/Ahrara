# go-smtp adaptation

Vendored from emersion/go-smtp **v0.25.0**, commit
`642c374442498d8e36bc67c27391b5700ac66b89`. Original MIT license retained.

Local changes are marked Ahrara or controlled by new options:

* Inclusive DATA limit: inspect the decoded terminator at the exact boundary.
  `TestAhraraInclusiveDataLimit` failed with 552 before this fix.
* `AbortOnDataError`: write the backend's SMTP error and close instead of
  draining an unbounded DATA stream. Default upstream behavior is retained.
* `DisableChunking`: disable both advertisement and handling of BDAT. Ahrara
  v1 uses DATA, including SMTP dot removal without CRLF normalization.

Run the vendored tests as well as the receiver integration tests when updating.
No network dependency is patched at build time.
