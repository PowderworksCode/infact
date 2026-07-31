# Fact packs

## Purpose

A fact pack contains everything Infact has derived about one subject. A
subject is either a language distribution or a library. A library has one
logical fact pack containing all available signatures, behaviors, effects,
types, and optional extractors.

Examples:

```text
rust-core
rust-itertools
rust-strum
```

Entl supplies observations about a codebase. Fact-pack rows and those
observations enter Infact's dataflow. Infact emits typed facts. Straitjacket
decides which facts are findings and how they affect the process exit status.

```text
Entl observations + fact-pack rows -> Infact dataflow -> facts -> policy
```

Parser packs and fact packs are different artifacts. Parser packs teach Entl
how to parse a language. Fact packs give Infact knowledge about a language or
library.

## Contents

A fact pack is an OCI artifact with a `pack.toml` manifest and zero or more
content blobs:

```text
pack.toml
api/
behaviors/
effects/
types/
extractors/
```

Data-only packs are normal. Executable extractors use Wasm; fact packs do not
contain native plugins.

The manifest identifies the logical subject, every source package or toolchain
input, and the derivation environment. A logical library can have several
source inputs; the Strum pack includes both `strum` and `strum_macros`. The
manifest also lists the fact capabilities and every content blob by media type
and SHA-256 digest. Paths are relative, normalized, and must not escape the
pack.

One logical pack can have multiple releases for one upstream version. For
example, `rust-itertools:0.15.0-r2` is the second Infact derivation release for
itertools 0.15.0.

## Sources

GHCR is a prebuilt cache, not an authority. The authority is the subject
source, its digest, the compiler and target inputs, the analyzer, and Infact's
derivation procedure.

Anyone can generate a compatible fact pack locally. Local, private-registry,
and public-registry packs use the same manifest and OCI layout. Publication is
always explicit. Infact and Straitjacket never upload private source or a
locally generated pack during analysis or synchronization.

The normal resolution order is:

1. A digest-pinned artifact in the local cache.
2. A compatible artifact in configured private registries.
3. A compatible artifact in the configured public registry, initially GHCR.
4. Local generation when the repository permits it.

Registry order is configuration. A repository may disable registries and
generate every pack locally. It may also require prebuilt artifacts and reject
missing packs.

## Compatibility

A matching name and library version are insufficient. Resolution compares all
inputs that can change derived facts:

- subject source digest;
- fact schema and analyzer digest;
- compiler name and version;
- target triple, CPU, target features, and relevant `cfg` values;
- enabled package features;
- proc-macro expansion digests where applicable.

Facts should retain conditions when practical instead of producing a physical
variant for every feature combination. When a compatible prebuilt variant
does not exist, the user can generate one for the resolved build context.

Machine-specific noise such as usernames, hostnames, temporary directories,
and absolute source paths is not part of a fact pack.

## Selection

Straitjacket selects fact packs from the codebase itself. Dependency packs
follow the languages and resolved dependencies already present in the
repository's lockfiles, so depending on a library is what asks for its facts.

```toml
[facts]
registries = [
  "ghcr.io/acme/infact-facts",
  "ghcr.io/zmaril/infact-facts",
]
dependencies = "automatic"
build-missing = true

[[facts.builders]]
ecosystem = "cargo"
command = ["company-infact-builder", "build"]
```

`dependencies = "automatic"` resolves a pack for every dependency the
repository's lockfiles declare. There is no separate list of libraries to
request: what the repository depends on is what gets described.

The consumer lockfile records resolved OCI manifest digests. Analysis uses the
lock without re-resolving tags. A locked run fails when configuration and lock
state disagree.

The lock is TOML and retains the complete validated pack manifest alongside the
OCI manifest digest and optional origin reference. This makes the lock useful
without contacting a registry and lets Infact detect a cache entry that does
not match the recorded provenance. Locked resolution rejects missing,
ambiguous, and unrequested entries instead of silently updating the selection.

## Local generation

Local generation follows the same deterministic pipeline used to produce
public artifacts:

1. Entl resolves the exact subject source and build context.
2. Language tools produce syntax, compiler, documentation, and LSP
   observations as available.
3. Infact converts observations into base relations.
4. DBSP derives recursive and joined facts.
5. The builder validates provenance and compatibility metadata.
6. The builder writes an OCI image layout into the content-addressed cache.
7. An explicit command may publish those exact bytes to an OCI registry.

The first source-backed core author implements steps 1–4 for selected Rust
standard-library modules:

```sh
infact effects rust-std /path/to/repository \
  --parser-path /path/to/parser-packs \
  --output /tmp/rust-std-effects.json
```

Its direct effect-origin classifications are explicit inputs. Calls and
source spans come from the installed standard-library source, and DBSP computes
transitive effects.

At repository-analysis time, verified `call-effects` catalogs seed fully
qualified external calls found in Rust source. Infact resolves local syntax
calls, propagates effects with DBSP, and emits evidence paths from each local
caller to the external effect origin. The catalogs remain portable pack data;
repository policy remains a Straitjacket concern.

`infact facts build` completes steps 5–6 for `cargo/core`. It validates the
requested version against the active compiler, emits the catalog as a
`call-effects` content blob, records compiler/source/analyzer provenance in a
canonical manifest, and writes one deterministic OCI image layout. The command
accepts the argument contract used by Straitjacket's configured local builder.
It never imports, locks, or publishes the artifact itself; those remain explicit
consumer operations.

Compiler operations may execute build scripts or proc macros. Local generation
must disclose that behavior and will use a restricted build environment. A
registry miss does not silently authorize dependency execution.

`infact-fact-builder` defines the external builder interface used by
Straitjacket. An explicitly configured command receives:

```text
--ecosystem <name> --package <name> --version <version>
--repository <path> --output <path>
```

It writes one OCI image layout at `--output`. The library imports that layout
through the normal verified cache path; the caller separately checks that its
subject satisfies the request. Builder execution requires `build-missing =
true`, never occurs during ordinary or offline analysis, and is disabled by
`--prebuilt-only`.

## Local cache

The local cache is a verified content-addressed store. OCI manifests, canonical
fact-pack configs, and content layers live under `blobs/sha256`; small entries
under `entries/sha256` identify installed manifest digests. A cache directory
has its own schema marker and Infact will not adopt a nonempty unmarked
directory.

Import checks the OCI layout and media types, descriptor sizes, every SHA-256
digest, manifest-to-layer correspondence, safe content paths, and canonical
`pack.toml`. Installation writes blobs before making the manifest visible.
Repeated imports are idempotent; a partial import can leave only unreachable
blobs.

Local resolution is exact over the subject, source inputs, analyzer
provenance, compiler, target, and package features. It also requires the pack
to provide every requested capability. Among compatible artifacts it selects
the highest derivation revision and rejects an ambiguous revision.

The author/debug CLI currently exposes local packaging and cache operations:

```text
infact facts package PACK_TOML --output OCI_LAYOUT
infact facts cache import OCI_LAYOUT --cache CACHE_DIRECTORY
infact facts cache list --cache CACHE_DIRECTORY
```

## Registry transport

Infact speaks the OCI Distribution protocol used by GHCR. Registry pulls accept
OCI image manifests only, require the Infact artifact type, bound manifest,
blob, and total artifact sizes, and verify every downloaded digest before the
cache entry becomes visible. Tags are discovery references. An expected digest
may constrain a pull, and lockfiles retain the immutable resolved digest.

Anonymous access is the default. Basic credentials and bearer tokens are
accepted through values supplied by the caller; the CLI reads secrets from a
named environment variable. Credentials are never written into the cache,
manifest, or lockfile.

```text
infact facts cache pull OCI_REFERENCE --cache CACHE_DIRECTORY
    [--expected-digest SHA256] [--lock LOCK_TOML]
infact facts lock add --lock LOCK_TOML --cache CACHE_DIRECTORY --digest SHA256
infact facts lock list --lock LOCK_TOML
infact facts lock verify --lock LOCK_TOML --cache CACHE_DIRECTORY
```

`lock add` is the bridge for locally generated and private artifacts: once the
same verified bytes are in the cache, their origin no longer changes how they
are selected or consumed.

## Initial fact relations

Entl observations include definitions, calls, types, implementations, imports,
dependencies, and source spans. Fact packs initially contribute callable
signatures, callable behaviors, direct effects, effect conditions, and subject
identity.

Infact initially derives:

- exact and near token clones;
- library behavior matches;
- direct and transitive effects;
- effect provenance paths;
- unresolved call targets and dependencies without compatible facts.

Effect propagation is a recursive relation:

```text
may-effect(function, effect) <- direct-effect(function, effect)
may-effect(caller, effect) <- calls(caller, callee), may-effect(callee, effect)
```

Infact reports the relation and its provenance. Straitjacket owns policy
such as prohibiting filesystem writes in a domain package or reporting a
library opportunity.

## Commands

The intended consumer interface is:

```text
straitjacket facts sync
straitjacket facts sync --prebuilt-only
straitjacket facts sync --offline
straitjacket facts build cargo:internal-library@1.4.2
straitjacket facts publish cargo:internal-library@1.4.2 --registry REGISTRY
straitjacket facts status
```

Infact implements validation, packaging, caching, locked resolution, and OCI
pulling as library APIs. Its CLI exposes those operations for pack authors and
diagnostics. Local dependency generation and explicit publication remain on
the delivery list. Straitjacket will be the normal repository-facing CLI.

## Delivery order

1. Define and validate `pack.toml`. (implemented)
2. Generate `rust-core` and `rust-itertools` locally. (checked `rust-core`,
   `rust-itertools`, and `rust-strum` packs implemented)
3. Store them as OCI layouts in a content-addressed cache. (implemented)
4. Pull digest-pinned prebuilt artifacts from GHCR. (implemented)
5. Record and verify immutable TOML lock selections. (implemented)
6. Consume local and locked fact packs from Straitjacket repository rules.
   (implemented for behavior opportunities and exact/near clone facts)
7. Publish the reference artifacts produced by the local builder.
8. Build missing dependency packs locally with explicit permission.
   (Straitjacket builder protocol implemented; general-purpose authoring
   coverage remains library-specific)
9. Add explicit publication.
10. Restrict compiler, build-script, and proc-macro execution.
11. Automate upstream release detection, fact diffs, review, and publication.

The first end-to-end fixture installs a dependency `rust-itertools` pack,
loads an Entl parser pack, and reports the resulting behavior opportunity
through Straitjacket. Text, JSON, SARIF, offline cache use, lock enforcement,
and cross-file suppression share the same reporting path. The checked
`rust-core` pack establishes typed call-effect data. Broader effect coverage,
private dependency authoring, and derivation explanations remain delivery
work.
