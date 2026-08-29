# Infact

> **Early and unstable.** Infact is under active development, has not been
> released, and is not ready for use by anyone outside this repository. Fact
> schemas, pack manifests, and crate interfaces change without notice or
> migration. Analyzer coverage is deliberately narrow and incomplete.

Infact derives typed, provenance-carrying facts from observations acquired by
Entl. DBSP maintains relations incrementally. Infact does not decide whether a
fact violates repository policy; Straitjacket owns that decision.

The first analysis finds repeated Tree-sitter token sequences. Formatting and
comments are excluded according to metadata in the active parser pack.

```sh
cargo run -p infact-cli -- duplication ../ordnung --config infact.toml
```

Near clones preserve syntax and identifier/literal equality patterns while
allowing consistent substitutions:

```sh
cargo run -p infact-cli -- duplication ../ordnung --config infact.toml --kind near
```

Exact clones are excluded from near-clone output. `max-changed-percent` limits
how much substitution a reported pair may contain.

The checked-in `infact.toml` discovers Entl's reference parser-pack directory.
An installed binary can point at any pack directory instead:

```sh
infact duplication . --parser-path /path/to/parser-packs
```

Machine-readable facts are JSON Lines:

```sh
infact duplication . --parser-path /path/to/parser-packs --jsonl
```

## Development

Infact reads Entl through Cargo path dependencies, so an
[entl](https://github.com/PowderworksCode/entl) checkout has to sit beside this
one:

```text
powderworks/
  entl/
  infact/
```

`scripts/dev.sh` checks for that sibling before anything else, points git at the
committed hooks, and builds the workspace along with the measurement harnesses
under `tools/`, each of which is its own workspace and so is not covered by
`--workspace`.

```sh
scripts/dev.sh

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Discarded errors

`infact-rust-errors` records the sites where a fallible expression's error is
dropped instead of returned: `let _ =`, `.ok()`, an `Err(_)` arm, an `Ok(..)`
binding with no error arm, `.filter_map(Result::ok)`, `.unwrap_or*`,
`.map_err(|_| ..)`, and `.unwrap()`/`.expect()`. Each fact carries the enclosing
callable and whether that callable returns `Result`, returns `Option`, or
cannot report a failure at all.

One signature is not the whole answer, so the callers are resolved too. A
discard inside an infallible callable that only infallible callables ever call
cannot be reported anywhere, and changing the immediate signature would not be
enough. Each fact carries that verdict and the chain of calls that would have
carried the failure. Calls resolve by name, and only when exactly one callable
answers to it, so an unresolvable caller is reported as unknown rather than as
sealed.

The analyzer resolves no types, so it reports how certain each site is rather
than guessing. `.ok()` and `Err(_)` name `Result` and nothing else;
`.unwrap_or_default()` reads the same on `Option` and is reported as possible.
Whether a given form is permitted is not decided here.

## Library behavior matches

`infact catalog` reduces rustdoc JSON to stable, typed API facts. The checked-in
`infact-packs/rust-itertools/api/itertools-0.15.0.json` contains 193 public callables from
`itertools 0.15.0`: 170 trait methods and 23 free functions.

```sh
cargo +nightly rustdoc --manifest-path /path/to/itertools/Cargo.toml \
  --lib -Z unstable-options --output-format json
infact catalog /path/to/target/doc/itertools.json \
  --package itertools --version 0.15.0 \
  --output infact-packs/rust-itertools/api/itertools-0.15.0.json
```

Catalog files are data. Infact configuration remains TOML:

```toml
[catalogs]
search-paths = ["infact-packs/rust-itertools/api"]

[behaviors]
search-paths = ["infact-packs/rust-itertools/behaviors"]

[macro-behaviors]
search-paths = ["infact-packs/rust-strum/macro-behaviors"]
```

Derive a behavior from library source:

```sh
infact behavior derive /path/to/itertools-0.15.0 \
  --callable itertools::Itertools::counts \
  --config infact.toml \
  --output infact-packs/rust-itertools/behaviors/itertools-counts-0.15.0.json
```

For `counts`, derivation follows the public method into
`counts_with_hasher`. The normalized body names the created map, iterator
input, bound item, entry key, and returned value. Loop operations are nested
inside `iterate`, so dataflow and control scope are explicit rather than
implied by a flat operation list. Behavior matching requires this derived
artifact as well as the compatible external signature.

Deriving a library's full behavior set produces a large amount of JSON that is
reproducible from the library source, so it is generated on demand rather than
committed. `infact-packs/` holds only what this repository's own tests and
documentation depend on; `/generated-packs` is ignored and is the place to build
a complete pack.

The checked-in family currently contains:

- `Itertools::counts`
- `Itertools::counts_by`
- `Itertools::into_group_map`
- `Itertools::into_group_map_by`
- `Itertools::sorted`
- `Itertools::sorted_by`
- `Itertools::sorted_by_key`
- `Itertools::sorted_unstable`
- `Itertools::sorted_unstable_by`
- `Itertools::sorted_unstable_by_key`

`behaviors` derives matches without editing source:

```sh
infact behaviors ../ordnung --config infact.toml
infact behaviors ../ordnung --config infact.toml --jsonl
```

The Rust behaviors recognize typed manual histograms and grouping maps. For
example, a histogram consists of a
`HashMap<K, usize>` initialization, a loop containing only
`*counts.entry(item).or_default() += 1`, and the same map returned afterward.
If the key is the item, it matches `counts`; if the key is derived from the
item, it matches `counts_by`. Equivalent `Vec` entry pushes match the two group
map methods. A smaller `collect::<Vec<_>>().join(separator)` matcher remains as
a regression case. The sorting family recognizes an iterator collected into a
temporary `Vec` and immediately sorted with the corresponding slice method.

The derived program states the library behavior, the syntax matcher finds that
program in local code, and the external catalog states the API contract.
Compiler-derived type evidence can strengthen these match facts later.

Proc-macro behaviors use the same evidence interface, but derive behavior from a
real expansion instead of a callable body. This records the behavior of a
small `strum::Display` probe:

```sh
infact behavior derive-macro /path/to/probe-crate \
  --type-name InfactProbe \
  --macro-package strum \
  --macro-version 0.28.0 \
  --derive-path strum::Display \
  --config infact.toml \
  --output infact-packs/rust-strum/macro-behaviors/strum-display-kebab-0.28.0.json
```

The checked macro artifacts cover `Display`, `AsRefStr` in kebab and snake
case, and `VariantArray`. String candidates require an exhaustive unit enum
mapping and the exact expansion-derived case conversion. When Serde already
declares `rename_all`, that convention selects the Strum artifact. Array
candidates require every unit variant exactly once and in declaration order.

## Rust standard-library effects

The trial standard-library author derives an effect catalog from the active
toolchain's installed `rust-src`:

```sh
infact effects rust-std . \
  --config infact.toml \
  --output /tmp/rust-std-effects.json
```

Entl observes the active compiler release, sysroot, and standard-library source
location. Infact parses selected `env`, `fs`, `net`, `process`, `thread`, and
`time` modules with the runtime Rust parser. Explicit effect-origin rules
produce direct seeds; DBSP propagates those effects through syntax-resolved call
edges to a fixed point.

Each public summary retains its evidence path. For example,
`std::env::var` traces through `_var`, `var_os`, and `_var_os` to
`env_imp::getenv`, including the source span of every call.

The command accounts for every call-shaped expression as an internal link,
known effect origin, constructor, call outside the selected modules,
dynamic or ambiguous method, or unknown. These categories are diagnostic;
only internal links participate in recursive propagation.

When a consumer enables call-effect analysis, `infact-analysis` applies those
catalogs to repository Rust source. Fully qualified external calls become local
effect origins, local syntax-resolved calls form the recursive relation, and
DBSP emits one evidence-bearing effect trace per reachable callable. Ambiguous
local calls that may reach an effectful target become analysis diagnostics.
Straitjacket consumes these traces to enforce capability policy.

This is deliberately incomplete. Call edges are not yet compiler-resolved,
`cfg` branches are not filtered for the active target, and ambiguous calls are
left unresolved. `--source` and `--version` can select another Rust checkout
without using the active toolchain.

The corresponding local Infact-pack builder implements Straitjacket's external
builder protocol:

```sh
infact facts build \
  --ecosystem cargo \
  --package core \
  --version "$(rustc --version | cut -d' ' -f2)" \
  --repository . \
  --parser-path /path/to/entl/parser-packs \
  --output /tmp/rust-core-oci
```

It currently accepts only `cargo/core`. The requested version must equal the
repository's active compiler. The generated manifest records the compiler
release and commit, a digest of every selected standard-library source, the
embedded analyzer digest, the catalog digest, and the derivation revision.
Rebuilding from identical inputs produces an identical OCI layout.

Straitjacket can invoke the same command when no compatible prebuilt artifact
is available:

```toml
[facts]
build-missing = true

[[facts.builders]]
ecosystem = "cargo"
command = [
  "infact",
  "facts",
  "build",
  "--parser-path",
  "/path/to/entl/parser-packs",
]
```

## Boundaries

```text
Entl observations -> Infact facts -> Straitjacket findings
```

Tree-sitter grammars are runtime-loaded Wasm artifacts. No language grammar is
linked into the Infact binary.

## Infact packs

One Infact pack contains all knowledge derived about a language or library:
signatures, behaviors, effects, types, and optional Wasm extractors. Infact packs
are OCI artifacts. GHCR provides public prebuilt artifacts, while local builds
and private registries use the same format. Publication is always explicit.

The checked `rust-core` pack records typed direct call effects for selected
standard-library APIs and is versioned against the Rust compiler release. New
compiler-specific variants can be produced by the source-backed local builder.
`rust-itertools` and `rust-strum` are library packs containing signatures and
behavior facts.

```text
ghcr.io/zmaril/infact-facts/rust-itertools:0.15.0-r1
ghcr.io/zmaril/infact-facts/rust-itertools@sha256:...
```

The first form selects a release. The second identifies its exact contents.
See [the fact-pack design](docs/infact-packs.md) for resolution, local generation,
compatibility, dependency selection, and the implementation sequence.

The first implemented authoring check validates `pack.toml`, safe content
paths, and every declared content digest:

```sh
infact facts validate /path/to/infact-pack/pack.toml
```

Analyzer source trees use a deterministic path-and-content digest:

```sh
infact facts hash /path/to/analyzer/source
```

A validated directory can be packaged into a deterministic local OCI image
layout without registry access:

```sh
infact facts package /path/to/infact-pack/pack.toml --output /path/to/layout
```

Import verifies the OCI index, manifest, canonical `pack.toml`, every descriptor
size, and every SHA-256 digest before installing blobs in the local
content-addressed cache. Reimporting the same layout is idempotent.

```sh
infact facts cache import /path/to/layout --cache /path/to/fact-cache
infact facts cache list --cache /path/to/fact-cache
```

The library API can load a manifest by OCI digest or resolve the highest pack
revision matching a subject, exact source and analyzer provenance, build
compatibility, and required fact capabilities.

OCI registries, including GHCR, use the same verification path. A mutable tag
may be used to discover an artifact, but `--expected-digest` can constrain the
pull and the resulting TOML lock always records the resolved manifest digest.

```sh
infact facts cache pull \
  ghcr.io/zmaril/infact-facts/rust-itertools:0.15.0-r1 \
  --cache /path/to/fact-cache \
  --lock /path/to/infact.lock.toml

infact facts lock verify \
  --lock /path/to/infact.lock.toml \
  --cache /path/to/fact-cache
```

Private registries accept either `--username` with `--password-env`, or
`--bearer-token-env`. Secret values are read from the named environment
variables and are not placed in arguments or lockfiles. `facts lock add`
records a locally generated or otherwise preinstalled artifact without registry
access.
