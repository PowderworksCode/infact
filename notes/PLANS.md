# Reducing the amount of code in infact — five plans

**This file moved into the repo on 2026-08-04.** It and `BRIEF.md` had been
sitting in the `powderworks-slim` working directory, which is not a git
repository, so they survived only as long as the box did. Edit them here.
Several sessions share this document; committing it means a change to it is
reviewable and recoverable like any other.

Written 2026-08-02 from the `~/powderworks-slim` checkout: entl `7400b8a`,
infact `2798037`, straitjacket `5925846`, all clean. Nothing in any repo was
modified to produce this document.

**Numbering note.** The plans below are numbered in the ranked order used in the
summary table at the bottom. An earlier draft numbered them in a different order
while drafting, which made "Plan 3" ambiguous between the Datalog plan and the
distribution-layer plan. This document uses the ranked numbering throughout and
is the authority: **Plan 3 is the distribution layer**, Plan 4 is Datalog.

---

# STATUS — updated 2026-08-03. Read this first.

| plan | state |
|---|---|
| 1 — Delete DBSP | **DONE by the ascent session.** infact `2f97ada`. Replaced by one `ascent` Datalog rule; −194 packages. Reach kept its BFS, later rewritten onto `pathfinding`. |
| 2 — Recognition → queries | **DONE for Rust and TypeScript.** See below. |
| 3 — Delete distribution layer | **NOT STARTED.** `oci-client` is now the only reason a static analyzer links an HTTP client. |
| 4 — Datalog core | **DONE as part of Plan 1**, at the scope that measured well. |
| 5 — Uniform term `Form` | **NOT STARTED, still believed wrong.** |

### Plan 2, as landed

- **Behaviors macro recognition** → `queries/behaviors.scm`. Merged.
  **+61 to +104 lines** — this was never a code reduction. It carried a real bug
  fix: `line_comment` is a NAMED child of `enum_variant_list`, so every
  doc-commented enum was invisible to macro matching.
- **`infact-rust-errors` → `infact-errors`**, query-driven. Merged.
  15 of 17 per-language functions deleted, node-kind literals 16 → 4.
  Behavior-preserving, verified against 782 golden rows: 612 comparable sites,
  zero lost/gained/changed.
- **TypeScript**, in review as entl#10 + the matching infact PR.
  **Zero new Rust for recognition** — the pass condition held. Containment
  needed one manifest field (`propagation = "unchecked"`) and five lines.

### The rule this work established

Mechanism follows problem shape. Measured across three sessions, not argued:

| shape | mechanism |
|---|---|
| recursive closure, no witness | ascent Datalog rule |
| recursive search returning a path | plain BFS / `pathfinding` |
| non-recursive join | plain Rust (`min_by_key`) |
| shape recognition | tree-sitter queries |
| tree→tree lowering (`Form`) | per-language Rust crate |
| cross-language equivalence | shared laws in `simplify.rs` |

**Queries express shape. They cannot count** — arity, universality, and ordering
stay in the consumer. Getting universality wrong yields a partial, confidently
wrong answer.

### Open, in rough priority

1. **A TypeScript corpus has never been audited against straitjacket's deny
   list**, which was tuned on Rust. Flagged unresolved in entl#10. Shipping the
   `.scm` is what switches a language on for every consumer already pointing at
   the pack directory.
2. **`[tests]` is unmodelled for TypeScript** — tests are marked by filename or
   a `describe` call, not an annotation, so `in_test` is always false and the
   "exempt tests" policy cannot work there. Genuinely unmodelled, not merely
   unexpressed.
3. **`infact-rust-effects`** — 2,419 src lines, only 7 `kind()` sites. Smallest
   remaining recognition surface; mostly catalog lookup.
4. **Plan 3**, owned elsewhere.

### Environment facts that contradict the docs

- **Do not prefix cargo with `RUSTC_WRAPPER=`.** sccache is configured globally
  with `incremental = false`; unsetting the wrapper disables the cache. entl's
  AGENTS.md caveat does not apply on this box.
- **infact pins rustc 1.97.1** in `rust-toolchain.toml` as of the dbsp removal.
  entl and straitjacket still use the 1.96.1 rustup default. The old "1.97.1
  ICEs on dbsp" warning is obsolete.
- **`gh pr create` needs `GH_HOST=github.com`** or it 403s on the proxy GraphQL.
  Pushing needs nothing special. `gh auth status` misreports the internal host.
- **`cargo fmt --all` run from infact reaches into entl** through path
  dependencies, and will silently reformat files there.

---

## Baseline: verified, with one correction

Every figure in the original brief reproduces exactly — 18,187 total lines /
15,866 src / 2,321 tests, 555 transitive dependencies, and the per-crate and
per-file tables.

**The correction is the DBSP dependency figure, and it changes what the plans
are worth.**

Measured by cutting edges in the `cargo metadata` resolve graph and re-walking
from the infact crates (normal + build edges):

| cut | reachable | removed |
|---|---|---|
| nothing | 555 | — |
| `dbsp` | 392 | **163** (29%) |
| `oci-client` | 519 | 36 (6%) |
| **both** | **248** | **307** (55%) |

DBSP alone accounts for 163 packages, not the 321 the brief attributed to it.
The brief's ~58% figure is only reachable by cutting DBSP **and** `oci-client`
together.

### The composition note (Plans 1 + 3)

163 + 36 = 199, but cutting both removes **307**. The gap is a **108-package
shared substrate** that neither dependency frees alone:

```
rustls, rustls-native-certs, rustls-pki-types, rustls-webpki, tokio-rustls,
aws-lc-rs, aws-lc-sys, ring, cmake, untrusted, zeroize, chacha20,
hyper, hyper-rustls, hyper-util, h2, http, http-body, http-body-util, httparse,
quinn, quinn-proto, quinn-udp, tower, tower-http, tower-layer, tower-service,
icu_collections, icu_locale_core, icu_normalizer, icu_properties, icu_provider,
chrono, url, idna, percent-encoding, form_urlencoded, base64, rand, digest,
security-framework, core-foundation, schannel, windows-* (14 crates), ...
```

That is a full TLS + HTTP + QUIC stack. `aws-lc-sys` invokes `cmake`, so a C
toolchain build is on the critical path. **A static analyzer that parses Rust
source currently compiles a QUIC implementation and AWS's libcrypto.**

Doing Plan 1 alone: 555 → 392. Plan 3 alone: 555 → 519. **Both: 555 → 248.**
Neither plan's dependency case is complete without the other; they should be
costed as a pair even though they are being executed separately.

### Two further corrections, both in the plans' favour

- **The DBSP footprint is larger than `grep dbsp` shows.** Four types carry
  14-line `SizeOf, Archive, Serialize, Deserialize, IsNone, archive_attr(..)`
  derive stacks — rkyv serialization required only so DBSP can spill batches to
  storage. That is ~60 lines containing no DBSP symbol, which a symbol grep
  misses entirely.
- **Both circuits are `Runtime::init_circuit(1, ..)`.** Single worker.
  `infact-rust-effects/src/lib.rs:1112` and
  `infact-duplication/src/engine.rs:210`. DBSP's parallelism is not bought
  either.

### Build cost, measured

Clean `cargo check`, separate `CARGO_TARGET_DIR`s, `RUSTC_WRAPPER=`, `-j 4`:

```
with dbsp     (infact-duplication)                          64.33 s
without dbsp  (infact-rust-errors + infact-rust-behaviors)  24.54 s
```

2.6×, and the without-DBSP side includes `infact-rust-behaviors`, the largest
crate in the repo.

---

## The DBSP question, settled

The brief asked whether the incrementality is load-bearing before recommending
either way. **It is not, and the two call sites fail for opposite reasons.**

### `infact-rust-effects::propagate_effects` — recursive, zero deltas

`crates/infact-rust-effects/src/lib.rs:1108`. Builds a fresh circuit, pushes all
calls and all seeds, calls `transaction()` **once**, consolidates, drops the
circuit. Three callers (`lib.rs:220`, `lib.rs:405`, `observed.rs:178`), all the
same shape.

`circuit.recursive` at `lib.rs:1115` — the incremental transitive closure the
brief correctly identified as the genuinely hard thing DBSP provides — is being
used as a fixed-point operator over input that never changes.

### `infact-duplication::ExactEngine` — incremental, not recursive

`crates/infact-duplication/src/engine.rs:210`. The circuit is
`indexed.join(&indexed, ..)` — a self-join on `(domain, digest)` emitting pairs.
No recursion at all; it is a group-by.

The retraction path (`self.input.push(window, -1)`, `engine.rs:254`) fires only
when a path is seen twice. The production callers are `lib.rs:92-95` and
`lib.rs:128-131`:

```rust
let mut engine = ExactEngine::new(config)?;
for file in tokenized.files {
    engine.replace(file)?;
}
```

Each file once, never revisited. The only code exercising retraction is two
tests at `lib.rs:209-259` that assert incremental equals fresh — the engine
self-testing its incrementality, not a consumer of it.

**Recursion without incrementality in one crate, incrementality without
recursion in the other. Nothing uses both.**

---

# Plan 1 — Delete DBSP

*Kind: replace the execution engine.*
*Status: not assigned as of writing.*

### What it deletes

- `propagate_effects`, `infact-rust-effects/src/lib.rs:1108-1157` (49 lines),
  plus the `CallRelation` / `EffectRelation` derive stacks at `1060-1106` (46
  lines).

  Replaced by a semi-naive worklist closure, written out in full to cost it
  honestly — **28 lines**, no new concepts:

  ```rust
  let mut callers: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
  for call in calls {
      callers.entry(call.callee).or_default().push(call.caller);
  }
  let mut closed = BTreeSet::new();
  let mut frontier: Vec<EffectRelation> = seeds.iter().map(..).collect();
  while let Some(r) = frontier.pop() {
      if !closed.insert(r.clone()) { continue; }
      for caller in callers.get(&r.callable).into_iter().flatten() {
          frontier.push(EffectRelation { callable: *caller, effect: r.effect });
      }
  }
  ```

- In `infact-duplication/src/engine.rs`: three derive stacks on `WindowRecord`,
  `WindowLocation`, `MatchSeed` (~48 lines); the circuit (16 lines); the
  `DBSPHandle` / `ZSetHandle` / `OutputHandle` fields; `advance()`; and the
  `seeds: BTreeMap<MatchSeed, i64>` weight bookkeeping, which exists only
  because output arrives as a Z-set delta.

**Net ~150–200 lines, and one dependency (163 packages).**

### What it adds

Nothing. No new dependency, no generated artifact, no new concept a reader must
learn. The only plan here with a strictly negative add column.

### What it breaks

`infact/AGENTS.md:6` — *"Its execution engine is DBSP"* — becomes false and must
be rewritten. That is a factual claim about the implementation, not a boundary.

It does **not** cross `infact/AGENTS.md:11` — *"Public facts must not expose
DBSP streams, batches, weights, or circuit types."* That boundary is precisely
what makes this a local change. The seam did its job before anyone knew what for.

### What it forecloses

Genuine incremental re-analysis: watch mode, an LSP server, re-running over a
changed file without rescanning. Nobody has this today and no config asks for
it. Also forecloses parallel evaluation — though `init_circuit(1, ..)` shows
that was never taken.

### Cheapest falsifying experiment

Replace **only** `propagate_effects`. It is self-contained, has three callers,
and one direct test at `lib.rs:1380`. Run the effects suite. ~1 hour. If it
passes, the duplication half is the same argument with more mechanical work.

Free second measurement while there: a batch group-by should be *faster* than N
per-file transactions. If it is not, that is a surprise worth knowing.

---

# Plan 2 — Recognition as parser-pack queries

*Kind: move logic into data.*
*Status: **OWNED — in progress this session.***

### What it deletes

**~800 lines of Rust, in two halves.**

**Half one, already planned in `entl/notes/todo.txt`:** ~210 of
`infact-rust-errors`' 745 lines of discard-form recognition → `queries/discards.scm`
in the entl parser packs. That note records a completed spike: `Query::new`
compiles against a wasm-loaded `Language`; the anonymous `_` pattern is matchable
as a quoted node; predicates work; and the negation problem (half the forms turn
on the *absence* of a binding — `Err(_)` vs `Err(e)`) is solved by quantified
captures, where the capture is simply missing from the match. The loader
(`ParserPack` query loading, `LoadedParser::matches`, `QueryMatch::has`) and the
`queries_sha256` provenance digest are both marked DONE and verified.

**Half two, not recorded anywhere before this document:**
`infact-rust-behaviors/src/lib.rs:426-793` is **367 lines**, and
`macro_derivation.rs` is **226 more** — 593 lines of hand-written Rust
recognizing exactly three strum derives. The functions are
`collect_enum_macro_matches`, `enum_serde_case`, `unit_enum`,
`manual_variant_array`, `manual_as_str`, `display_delegates_to_as_str`,
`mappings_match_case`, `only_expression`, `impl_function`, `is_impl_for`,
`named_descendant`, `last_named_identifier`. This is node-kind pattern matching
with almost no computation in it.

### The argument that matters more than the line count

`infact/notes/todo.txt`, under "What NOT to spend time on":

> A table of adaptor name -> operation. It would work and it is exactly the
> per-library knowledge the design exists to avoid.

The macro-recognition path already **is** that table — written as 593 lines of
Rust rather than as a table, which is the expensive spelling of the same thing
the note rejects.

### What it adds

`.scm` query files in entl's parser packs, plus per-form `absent = [..]`
declarations in `parser.toml`. A reader must learn tree-sitter query syntax.
**Zero new Rust dependencies.**

### What it breaks

No boundary. `infact/AGENTS.md:26-27` — *"Tree-sitter grammar acquisition and
parsing belong to Entl. Infact may interpret concrete syntax trees and parser
metadata"* — is exactly this shape. The work touches entl's parser packs, which
is where `AGENTS.md` says query data belongs.

### What it forecloses

Recognition that needs arbitrary computation. `entl/notes/todo.txt` already
scoped this honestly and lists what cannot move, and it should not be
relitigated:

- **`Certainty`.** `.ok()` proves a `Result`; `.unwrap_or_default()` reads the
  same on `Option`. Which forms are provable is a fact about the language's
  stdlib, not a pattern.
- **The query-result exclusion list.** `binary_search*` and
  `Path::strip_prefix` return `Result` where `Err` is an *answer*, not a
  failure. Reporting those buries real findings — it was ~10 of the first 75.
- **Return-type classification.** `Result` vs `Option` vs neither, from captured
  type text, is a per-language name list.

One to add, found in this survey: `mapping_is_exhaustive`
(`behaviors/src/lib.rs:563`) compares a variant set against a mapping set. That
is set logic, not a pattern.

Roughly 400 of `infact-rust-errors`' 745 lines stay Rust regardless — the call
graph, `resolve_reach` / `search_upward`, fact assembly, provenance. Reach is a
graph search, not a pattern match. This plan **shrinks** the crate to a
query-driven front end plus a graph; it does not remove it.

### Cheapest falsifying experiment

entl's note already spiked the errors half against the checked-in rust pack.

For the behaviors half: write **one** query for `manual_as_str` and run it
against `infact-core/src/lib.rs`, which contains **five** hand-written
`as_str()` impls — `DiscardForm` (:164), `Containment` (:194), `Certainty`
(:220), `Reach` (:248), `Effect` (:300). Real positives sitting in this repo.
If one query finds all five and does not fire on anything else, the pattern
generalizes. One afternoon.

*(An earlier draft of this document said six. It is five — verified by
`grep -c 'fn as_str'`, which also reports 5 across the whole of `crates/*/src`.)*

### RESULT — experiment run 2026-08-02, and all five recognizers migrated

Done in a scratch probe against a COPY of entl's rust pack; no repo modified.

**The experiment passed.** One `as_str` query over 217 files across all three
repos: 12 sites recognized, **0 false positives, 0 false negatives**. All 16
`fn as_str` accounted for — the 4 non-matches are newtype accessors
(`&self.0`) that the Rust recognizer also declines. `#eq?` is confirmed
enforced by `LoadedParser::matches` (a decoy with identical structure and a
different name does not match).

All five recognizers now run from `queries/behaviors.scm`: `manual_as_str`,
`display_delegates_to_as_str`, `unit_enum`, `manual_variant_array`,
`enum_serde_case`. Against the ground truth in
`infact-rust-behaviors/tests/strum.rs` (which asserts exactly 4 matches) the
query version reproduces them exactly — Display×1, AsRefStr×2, VariantArray×1,
on the same enums, with `NotExhaustive` correctly rejected.

**Three constraint kinds cannot move, and they are all counting:**

| kind | example | without it |
|---|---|---|
| arity | `only_expression` (body has one named child) | false positive |
| coverage | "every arm/variant matched" | silently partial mapping |
| ordering | `manual_variant_array` compares a sequence | wrong match |

Roughly 40 lines of residual Rust. The rule to carry forward: **queries express
shape; counting stays in Rust.**

**THE LINE-REDUCTION CLAIM IS FALSIFIED FOR THIS HALF. Measured after landing,
not predicted:**

```
recognition region in infact-rust-behaviors/src/lib.rs
  before   302 lines of Rust
  after    299 lines of Rust          delta  -3
  plus     107 lines of behaviors.scm (64 non-comment)

  NET: +61 to +104 lines.
```

I predicted ~220 deletable lines from this block. The real number is **3**. This
is the fifth entry in the note's list of confident predictions the corpus
disagreed with, and it is mine.

Why: the original recognizers were compact because each one walked and
extracted in a single pass (`manual_as_str` is 31 lines and does both).
Replacing them costs the same work in a different place — pull captures out of
matches, group by the enclosing node, apply the residual counting checks. The
orchestration around them (artifact filtering, the enum↔impl join, fact
assembly) was never recognition and never moved.

**So Plan 2 should not be sold on line count.** What it actually buys:

1. **Recognition became data.** This is what entl's note argued for and it is
   the real case: another language's pack ships its own `queries/*.scm` and
   needs no Rust. That is the TS/Zig/Go discard-form story.
2. **A latent bug got fixed** (below), which the query form made visible.
3. Node-kind literals moved out of Rust and into the pack.

### The errors half — GATE MEASURED ON TWO FORMS, and it says stop

`let_declaration` (33 lines, covers `LetUnderscore` + `OkBinding`) and `err_arm`
(25 lines — the binding-absence case entl's note called "THE ONE THAT
MATTERED"). Both express cleanly as queries; verified against a fixture:
`let _ = w()` and `let _ = w()?` match, `let x = w()` does not, `let Ok(v) = ..
else` matches, `let Ok(v) = ..` without an else does not, `Err(_)` matches and
`Err(e)` produces the quantified `@bind` capture so the caller drops it. The
absence trick works exactly as entl's spike recorded.

**The economics still do not work:**

```
Rust deleted   60  (let_declaration 33, err_arm 25, inspect dispatch 2)
Rust added    ~40  (form dispatch, bind check, QUERY_RESULTS exclusion,
                    expression extraction, scope lookup)
query added    24  non-comment (42 with comments)
                    -----
NET            60 -> 64.  Break-even, marginally worse.
```

**And the errors half has an extra structural cost the behaviors half did not.**
`walk` (lib.rs:172) fuses scope tracking with recognition: it threads a `Scope`
carrying `callable`, `span`, `containment`, `in_test` down the recursion, so
attributing a discard to its callable is *free* at the point of recognition. A
query returns discard nodes with no scope at all, so a query-driven version must
either join every match back to a scope by byte containment, or re-derive
`containment_of` / `has_test_attribute` / `simple_type_name` per match. The walk
cannot be deleted either way, because `resolve_reach` needs the `CallableNode`s
it builds.

**Recommendation: do not continue the errors half for line count.** The only
case that survives measurement is multi-language — if infact is to recognize
TS/Zig/Go discard forms, moving recognition into packs is the enabling step and
the line count is beside the point. If infact stays Rust-only, this is cost
without return.

### THE PROJECT IS GOING MULTI-LANGUAGE — which reverses the above

The recommendation to stop was right for one language and wrong for the target.
I measured the per-language cost at N=1, where amortization is invisible by
construction. The measurement was correct; the question was.

**Split of `infact-rust-errors`, measured function by function:**

```
PER-LANGUAGE, replicates for every new language     385 lines
  walk 61, record_call 35, walk_children 23, inspect 31, collect_file 24,
  method_call 48, let_declaration 33, err_arm 25, containment_of 16,
  attributes 20, let_condition 14, tuple_struct_name 12, simple_type_name 11,
  is_query_result 10, closure_binds 7, first_closure 6, has_test_attribute 3,
  is_test_module 3, trailing_segment 3

LANGUAGE-NEUTRAL, written once                      211 lines
  analyze_repository_errors 53, resolve_reach 45, search_upward 39,
  repository_module 18, source_span 14, discard_derivation 14,
  input_evidence 10, truncate 8, analyze_file 7, node_text 3
```

**The gate — can the WALK be query-driven? — was tested against an oracle and
it passes.** `infact_rust_errors::analyze_file` is public, so a query-driven
implementation can be diffed against the real one on real code:

```
218 files across all three repos          11 agree, 0 differ
scope-stress fixture                      17 agree, 0 differ
```

The stress fixture covers free functions, fallible/optional/generic return
types, inherent impls, trait impls, generic trait impls, doubly-nested modules,
`#[cfg(test)]` modules, `#[test]` functions, nested functions, closures, and
`Err(_)` / `Err((a, b))` / `Err(ref e)` / `let Ok(..) else`. Callable path,
containment, and `in_test` match the real walk exactly in every case.

**Measured architecture.** Scaffolding (callables, impls, modules, attributes,
call edges) comes from `queries/callables.scm`; forms from
`queries/discards.scm`; and the association between them is **byte-range
containment**, which is a property of trees, not of any language. The
language-neutral core is **153 lines** and names no Rust node kind. What stays
per-language is the two query files (42 lines) plus four data constants —
result/option type names and the test markers — which belong in `parser.toml`
exactly as entl's note says.

```
                     current            query-driven
  1 language            596                    454      -142
  2 languages           981                    499      -482
  4 languages         1,751                    589    -1,162
```

**The second language pays for it twice over.**

**A correction to entl's note.** Its recorded spike gives the negation trick as
`(tuple_struct_pattern type: (identifier) @t (identifier)? @bind)`. That
`(identifier)?` is subtly wrong and produces FALSE POSITIVES on real code:
`Err((pattern, message))` binds through a `tuple_pattern`, not an identifier, so
`@bind` is absent and a bound-and-used error reads as discarded. Found in
`entl-codebase/src/discovery/mod.rs:13726` by the oracle diff — never by
reading. The fix is a quantified wildcard, `(_)? @bind`, which is verified
working. Anyone writing `discards.scm` from that note will reproduce the bug.

**A pre-existing bug found by the migration.** `line_comment` nodes are NAMED
children of `enum_variant_list`. Today's `unit_enum` (lib.rs:548) requires
*every* named child to be an `enum_variant`, so **any enum whose variants carry
doc comments is silently invisible to macro-behavior matching.** The strum
fixture has zero doc comments, so no test exercises it. The query version fixes
this as a by-product and surfaces `DiscardForm` in infact's own `infact-core` — a real
match the current code cannot see. This is a behavior change and needs a
deliberate decision, not a silent fix.

**A latent inconsistency, not changed.** The AsRefStr path guards case
ambiguity (`preferred_case.is_some() || matching.len() == 1` — the
"don't invent certainty" rule); the Display path has no such guard. It does not
bite today only because a single kebab Display artifact ships. An enum with
all-single-word variants (`TaskKind`: Test/Lint/Format/Typecheck/Build) is
kebab/snake ambiguous and would get two contradictory Display matches if a
snake Display artifact were ever added.

**Footgun for the pack conventions.** A `#eq?` placed after a pattern's closing
paren compiles cleanly and silently becomes its own pattern matching every
node — 3,716 spurious zero-capture matches against 28 real ones. "Query
compiles" is not "query is attached to anything."

---

# Plan 3 — Delete the OCI / registry / cache / lock distribution layer

*Kind: delete whole crates. **Crosses a named boundary.***
*Status: **assigned to a separate session in its own checkout.***

### What it deletes

`infact-fact-pack` (2,015 lines) + `infact-fact-registry` (440) = **2,455
lines, 13.5% of the codebase**, and two crates. Removes `oci-client` (36
exclusive packages) and, composed with Plan 1, unlocks the 108-package shared
substrate described above.

### What was measured, and it is the whole argument

**There is no `straitjacket.lock.toml` in any of the three repos.** Infact's own
`infact.toml` reads its packs from plain local directories:

```toml
[catalogs]         search-paths = ["infact-packs/rust-itertools/api"]
[behaviors]        search-paths = ["infact-packs/rust-itertools/behaviors"]
[macro-behaviors]  search-paths = ["infact-packs/rust-strum/macro-behaviors"]
```

The content those 2,455 lines exist to distribute is **19 JSON files, 416 KB** —
and it is not being distributed through them. `FactPackCache`,
`build_oci_layout`, `OciDescriptor` / `OciManifest` / `OciIndex` / `OciLayout`,
`persist_noclobber`, and the registry transport serve a path nothing in the
fleet takes.

### What replaces it

A content-addressed directory plus a `pack.toml` listing sha256s. **Keep digest
verification** — ~40 lines, and the part carrying the security property. Delete
the OCI layout writer, the descriptor/index types, the blob cache, and the
transport. The OCI *encoding* is what goes; the integrity guarantee survives.

### What it breaks — explicitly

`infact/AGENTS.md:28-34` is six lines of declared boundary:

> Fact packs use OCI artifacts. GHCR is a prebuilt cache, not an authority.
> Users can generate identical artifacts locally or use another public or
> private OCI registry. Publication is always explicit. Lockfiles pin manifest
> digests, not mutable tags.
> Registry pulls and local imports enter through the same digest and manifest
> verification boundary. Pulling never implies publishing. Registry secrets must
> not appear in command arguments, manifests, lockfiles, or diagnostics.

**This plan crosses all of it.** The argument for crossing: those lines describe
a distribution story with zero users today, and the property they protect — you
get the bytes you asked for, verified — survives in a content-addressed
directory.

This is the one plan that trades away a product capability rather than an
implementation detail. It is a judgment call for the user, not for whoever
implements it.

### What it forecloses

Shipping prebuilt packs to third parties who do not want to run the deriver.
That is real, and it is why this ranks third rather than second.

### Cheapest falsifying experiment

Regenerate the three checked-in packs from source with the CLI and time it. If
it is under a minute, the cache is not earning 2,455 lines.

**The honest counterweight, from `infact/notes/todo.txt` itself:** the 494-crate
dependency corpus takes ~8 minutes. If real use looks like that rather than like
three packs, caching is load-bearing and this plan shrinks to "delete the
registry transport, keep the cache" — still ~440 lines and 36 packages, and
still worth doing.

---

# Plan 4 — Datalog for the recursive relational core

*Kind: new query engine, new dependency. The superset of Plan 1.*
*Status: not assigned as of writing.*

Someone correctly diagnosed that infact's hard problems are recursive relational
queries, then reached for a streaming database that solves them incrementally at
the cost of a QUIC implementation. **The diagnosis was right; the engine is a
category error.**

### What it deletes

Everything Plan 1 deletes, plus code Plan 1 leaves alone because hand-writing it
is exactly as bad as what is there now:

| target | lines |
|---|---|
| `infact-rust-errors` `resolve_reach` + `search_upward` (lib.rs:338-432) | 94 |
| `infact-rust-effects` `evidence_path` BFS (lib.rs:1011) | 52 |
| `infact-rust-effects` `propagate_effects` + derives | 95 |
| `infact-rust-behaviors` `derived_from` / `delegates_to` / `is_plainer` (lib.rs:293-344) | 51 |
| `infact-duplication` self-join + weight bookkeeping | ~110 |

~400 lines of hand-rolled fixpoint, BFS, and worklist code — **12 distinct
`VecDeque` / `visited` / `frontier` sites** across the crates — collapse into
one Datalog program of perhaps 40 rules. Reach resolution *is*
`reach(D, ancestor) :- discard(D, C), calls(P, C), fallible(P).` Effect
propagation is two rules. Both are currently prose.

### What it adds

One dependency — `ascent`, a proc-macro compiling Datalog rules to plain Rust at
build time. And a real new concept: contributors must read Datalog. That is a
genuine cost and the plan's main risk after the dependency claim itself.

### What it breaks

The same `AGENTS.md:6` rewrite as Plan 1. Nothing else.

### What it forecloses

Fine-grained control over evaluation order and early exit. `search_upward` can
stop at the first fallible ancestor; a Datalog engine computes the whole
relation. For a per-repository call graph that is almost certainly free, but it
is a real loss of control.

### Cheapest falsifying experiment — RUN, by the parallel `ascent` session, and it PASSES

Measured in `~/powderworks-ascent` (a separate checkout) and independently
re-measured here from its `cargo metadata`:

```
ascent, default features            43 packages
ascent, default-features = false    28 packages   <- what they adopted
exclusive to ascent, in tree        15 packages
WHOLE TREE                     555 -> 361         -194
```

Better than Plan 1 alone (-163), because dropping DBSP also drops the direct
`rkyv`, `feldera-size-of`, and `feldera-macros` dependencies. Their workspace
tests pass with zero failures. The gate that could have killed this plan
("if it brings 80 packages it dies") is cleared by a wide margin.

### What their three experiments established — a reusable rule

They applied ascent to three different problems and got three different answers.
This taxonomy is the durable finding, worth more than the line counts:

| shape | outcome | case |
|---|---|---|
| **Recursive closure, no witness needed** | **ascent wins big** | effects: a DBSP circuit + 46 lines of rkyv/SizeOf/IsNone derives became ONE rule line |
| **Recursive search that must return a path** | **ascent loses** | reach: +42 lines, the BFS still needed, result redundant — REVERTED |
| **Non-recursive join** | **plain Rust wins** | duplication: a `HashMap` group-by, no Datalog at all |

The effects rule in full:

```rust
ascent::ascent! {
    struct EffectClosure;
    relation calls(u64, u64);
    relation has_effect(u64, u8);
    has_effect(caller, *effect) <-- calls(caller, callee), has_effect(callee, effect);
}
```

**Why reach lost, and it generalizes:** `search_upward` does not merely answer
"is there a fallible ancestor" — it returns the *path* of call edges as
evidence. Datalog yields the relation, not the derivation that produced it, so
recovering the witness needs the BFS anyway and the rule becomes dead weight.
Any query here that must carry provenance has this property.

**Consequence for Plan 2's multi-language core:** the byte-range containment
join ("innermost callable containing this point") is category three —
non-recursive, plus an argmin. `.filter().min_by_key()` is three lines; Datalog
needs negation or an aggregate to say "innermost" and would be longer. **Ascent
does not help the scaffolding walk.** Reach stays a BFS, and in the
multi-language design it is language-neutral, so it is written once regardless.

---

# Plan 5 — Collapse `Form` to a uniform term algebra

*Kind: change the model.*
*Status: not assigned. **This is the plan most likely to be wrong.***

`Form` (`infact-normalize/src/lib.rs:101-243`) is a 26-variant enum. Fifteen
recursive methods walk it, each a full match. Measured:

```
151  map_children      145  Display        56  children
 25  anchors            23  holes          23  references_local
 17  depth              16  size
───
456  lines of per-variant traversal     + 142  the enum definition
```

`Term { head: Head, children: Vec<Term> }` makes `children`, `size`, `depth`,
`holes`, `anchors`, `map_children`, `contains`, `occurrences`, and `Display`
**one generic implementation each, ~5 lines apiece**; matching becomes
first-order unification. Estimated 400–600 lines. It would also resolve the
1,849-line file-size violation *by construction*, rather than by the three-way
split the note plans.

The tell that this is natural: `Display` already emits S-expressions, and the
note's worked examples are written as
`(do (traverse s x (select SCRUT (None) => ...)))`. **The enum is a typed skin
over a term language that already exists in serialized form.**

### What it adds

A `Head` symbol enum and an arity table — arity moves out of the type system and
into data, checked at runtime rather than at compile time.

### What it breaks

No boundary. Blast radius is real but bounded: 58 uses across 24 variants in
`infact-rust-normalize`, 15 across 13 in `infact-rust-behaviors`, and **zero**
in `infact-core`, `infact-cli`, and `infact-analysis`.

### Why it is probably wrong

`infact/notes/todo.txt` documents eight bugs in a single session that were all
one pattern — *a distinction erased in normalization* — each producing hundreds
to thousands of confidently wrong findings, **none visible in the output**.
`true`/`()` cost 1,390 false matches. `map`/`filter_map`. `next`/`next_back`.
`(k, _)`/`(_, v)`. `Self`. Arm order.

The typed enum is exactly what makes `Sift` and `Transform`, or `Traverse` and
`Accumulate`, un-confusable — different field sets, compiler-enforced. A uniform
term makes that entire bug class easier to write and harder to see, in the one
part of this codebase with a documented history of producing confident wrong
answers silently. Trading 500 lines for that is a bad trade.

The note's own warning about weak confidence in predictions — four consecutive
mispredictions where the corpus disagreed with the reasoning — applies here more
than anywhere else.

### What it forecloses

Compile-time exhaustiveness. Adding a `Form` variant today produces a list of
every site that must handle it. Afterwards it produces nothing.

### Cheapest falsifying experiment — and it probably replaces the plan

Keep the enum. Implement `map_children` generically **once** (via `children()`
plus a rebuild, or a small derive), then rewrite `size`, `depth`, `holes`,
`anchors`, and `references_local` in terms of it. If that alone recovers most of
the 456 lines, **the win arrives with none of the risk** and the plan is
correctly dead.

Run `crates/infact-rust-behaviors/tests/collisions.rs` afterwards — the note
calls it "the cheapest bug detector in the project" and it is the exact guard
for this failure mode.

---

# Ranking

| # | plan | lines | deps | risk | status |
|---|---|---|---|---|---|
| 1 | **Delete DBSP** | ~175 | −163 | low | unassigned |
| 2 | **Recognition → queries** | ~~800~~ **+61 measured** | 0 | low | **landed, behaviors half** |
| 3 | **Delete distribution layer** | ~2,455 | −36, unlocks 108 | medium — crosses a boundary | separate session |
| 4 | **Datalog core** | ~400 | +1, −163 | medium — unmeasured dep | unassigned |
| 5 | **Uniform term `Form`** | ~500 | 0 | **high — probably wrong** | unassigned |

**Do first: Plan 1.** Best ratio in the set and the only one that adds nothing.
It removes 29% of the dependency graph and 62% of clean check time for a fixed
point that is stepped once and a self-join that is never retracted. The
falsifying experiment is one function and an hour.

**Probably wrong but worth keeping: Plan 5.** The 456-line measurement is real
and the S-expression `Display` is genuine evidence the model wants to be a term
language. But it aims a type-safety reduction squarely at the one subsystem with
a documented eight-bug history of erasing distinctions invisibly. Its own
falsifier likely converts it into a safe refactor, which is the right outcome.

### A sixth, not written up

`infact-duplication` is 1,400 lines and one of the two DBSP consumers, and
token-based clone detection is the project's weakest, most commodity capability
— the behavior engine is the actual thesis. **Deleting it** rather than
de-DBSP-ing it is worth a moment's thought before Plan 1 does the work. Not
written up as a sixth plan because it duplicates Plan 3's kind (delete whole
crates).
