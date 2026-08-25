# Catalog fixtures

One catalogued callable each, copied verbatim out of a generated catalog.

The standard library's catalog is not committed — it is 26 MB of generated JSON,
reproducible in two seconds from a rustup component, and
`infact-packs/rust-std/README.md` says how. A test that needs to read a real
signature therefore reads it from here rather than from a pack that may not have
been built yet.

They are excerpts, not catalogs: a single `ExternalCallable`, with no digest or
version around it, because a digest over one callable would claim to be the
digest of the library it came from.

    slice-is-sorted.json   core 1.100.0-nightly, rustdoc format 61
