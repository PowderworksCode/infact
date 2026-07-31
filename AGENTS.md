# AGENTS.md

## What Infact is

Infact derives typed facts from observations acquired by Entl. Its execution
engine is DBSP. Straitjacket and other consumers decide which facts constitute
findings or policy failures.

## Boundaries

- Public facts must not expose DBSP streams, batches, weights, or circuit types.
- Facts retain source spans and derivation provenance.
- Keep output deterministic and paths codebase-relative.
- Infact does not assign severity, enforce thresholds, suppress results, or
  write remediation prose.
- External API catalogs are generated data. Matchers must verify the callable
  signature they rely on before deriving a library-behavior match.
- Derived behavior artifacts must retain library implementation spans and source
  digests. Runtime matching must bind them to the exact catalog version and
  digest from which they were derived.
- Proc-macro behavior artifacts must be derived from parseable expansions and
  retain the expansion digest. Runtime matching consumes the artifact; it does
  not execute the macro.
- Library-behavior facts identify matches; they do not edit source or prescribe
  a patch.
- Tree-sitter grammar acquisition and parsing belong to Entl. Infact may
  interpret concrete syntax trees and parser metadata.
- Fact packs use OCI artifacts. GHCR is a prebuilt cache, not an authority.
  Users can generate identical artifacts locally or use another public or
  private OCI registry. Publication is always explicit. Lockfiles pin manifest
  digests, not mutable tags.
- Registry pulls and local imports enter through the same digest and manifest
  verification boundary. Pulling never implies publishing. Registry secrets
  must not appear in command arguments, manifests, lockfiles, or diagnostics.
- Configuration files are TOML.

## Build and test

```sh
RUSTC_WRAPPER= cargo fmt --all --check
RUSTC_WRAPPER= cargo test --workspace
RUSTC_WRAPPER= cargo clippy --workspace --all-targets -- -D warnings
```

Do not commit, push, or publish unless asked.
