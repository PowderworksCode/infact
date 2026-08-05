# Language parity across entl and infact — the census

Taken 2026-08-05 against `entl 193f475`, `infact 9d542b5`, `straitjacket ce5ea0a`,
all clean and level with origin/main. Nothing was modified to produce it.

This lives in `infact/notes/` for the same reason the transfer notes do: it spans
all three repos, and the session directory it was written in is not a git
repository.

**Everything below was run, not read.** Where a claim comes from reading code
rather than from executing it, it says so.

---

## STATUS — read this first

The census describes the fleet as it stood when it was taken. Work started from
it immediately, so some of what it reports as a gap is in review.

| gap | state |
|---|---|
| `.tsx` gets no discard analysis | **in review**, entl#19, with `DivergentPacks` so it cannot recur |
| JavaScript gets no discard analysis | **in review**, entl#20, stacked on #19 |
| the one finding turning JavaScript on produces | **in review**, infact#33 |
| Python `[error-handling]` is blocked | **corrected** in entl#19: the note's stated blocker no longer holds. `discards.scm` is the remaining work |
| everything else | unstarted |

---

## How it was measured

Three things were actually executed:

1. **A polyglot fixture through straitjacket.** One file per language, each
   holding the same three things — a discarded error, a duplicated block, a
   `TODO` — scanned with `straitjacket --config <fixture>/sj.toml`. This is the
   only way to see what a repository in language L *actually gets*, as opposed
   to which crates exist.
2. **A pack-load experiment.** The TypeScript `.scm` files were copied into a
   scratch copy of the `tsx` and `javascript` packs and the scan re-run, to
   measure what the missing queries cost and whether they are the only thing
   missing.
3. **The three test suites.** entl green, infact green, straitjacket 1 failure —
   `clone_exclusions_scope_repository_rules`, pre-existing and unrelated.

---

## The end-to-end matrix — what a repository in each language actually gets

This is the table that matters, and it is not the same as "which crates exist".
`✓` was observed firing. `○` means the code exists and no production caller can
reach it. `—` means absent.

| capability | mechanism | rust | ts `.ts` | ts `.tsx` | js | python | zig |
|---|---|---|---|---|---|---|---|
| detect / inventory | `entl-codebase` profiles | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| parse | parser pack | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| exact / near clones | `[tokenization]` + `infact-duplication` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| text rules (todo, size, nesting…) | straitjacket + profiles | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| normalize to `Form` | per-language crate | ✓ | ✓ | ✓ | ✓ | ○ | — |
| library behaviors — derive | `infact-behaviors` + frontend | ✓ | ✓ | ✓ | ✓ | — | — |
| library behaviors — match | `analyze_repository` | ✓ | ✓ *(no pack)* | ✓ *(no pack)* | ✓ *(no pack)* | — | — |
| error discards | `infact-errors` + `.scm` | ✓ **8 forms** | ✓ **3 forms** | **✗** | **✗** | — | — |
| call effects | `infact-rust-effects` + catalog | ✓ | — | — | — | — | — |
| pointer ownership | `infact-zig-lifetimes` | n/a | n/a | n/a | n/a | n/a | ○ |
| semantic observations | provider | ✓ | ○ | ○ | ○ | — | ○ |

Three cells in that table are not what the crate inventory predicts, and each is
a finding rather than a gap:

### `.tsx` gets less than `.ts`, from the same bytes, silently

`tsx` is **not a language**. `parser-packs/tsx/parser.toml` declares
`language = "typescript"` with `grammar-name = "tsx"`; `entl-codebase` has one
`typescript` profile covering `ts, tsx, mts, cts` and no `tsx` id at all. So
`.tsx` and `.ts` are one language read by two grammars — and the `tsx` pack
ships no queries and no `[error-handling]`.

Measured. `b.ts` and `c.tsx`, byte-identical:

```
b.ts     3 error-discard findings
c.tsx    0
```

`infact-errors` skips it in silence, and that is deliberate — the doc comment
says a pack shipping no discard queries "describes no discard forms for its
language, which is a different thing from a language having none". That reason
is right for a language and wrong here: the language *is* described, by the
other pack.

**The fix is smaller than the gap.** Copying `typescript/queries/*.scm` into the
`tsx` pack: both queries compile against the tsx grammar unchanged and all three
findings appear. But the `[error-handling]` block must come too, or the verdict
silently differs on identical code:

```
b.ts   ... readOne is fallible    and discards `catch { }`
c.tsx  ... readOne is infallible  and discards `catch { }`   <- no propagation = "unchecked"
```

That is this project's signature failure — a distinction erased upstream,
invisible where it lands — reached from a new direction.

### JavaScript cannot reuse TypeScript's queries, and the failure is loud

The same experiment against the `javascript` grammar:

```
parser pack "tree-sitter-javascript" query "callables" does not compile:
  at row 10, offset 406: "type_annotation"
```

`tree-sitter-javascript` has no `type_annotation`, so `callables.scm` needs a
JS-specific copy with the return-type patterns dropped. `discards.scm` needs
nothing — `catch_clause`, `void`, and `.catch(..)` are all shared. **The pack
load fails rather than matching nothing**, which is the check
`every_checked_in_pack_declares_only_kinds_its_grammar_has` was built for
working exactly as designed.

### The behaviors matcher already covers `.tsx` and `.js`; the errors path does not

`infact-ts-behaviors` does not use queries at all — it walks node kinds through
`infact-ts-normalize` and gates on
`is_ecmascript`: `language.id == "typescript" || "javascript" || "tsx"`. Since
`.tsx` resolves to the `typescript` profile and `.js` to `javascript`, both are
already covered. (The `== "tsx"` arm is dead: no profile has that id.)

So the ECMAScript family is **split by mechanism, not by language**: the
walk-based capability covers all three extensions, the query-based one covers
one. Both are inert in practice because no TypeScript fact pack is checked in —
`infact-packs/` holds `rust-core`, `rust-itertools`, `rust-strum` and nothing
else.

---

## Corrections to the census I was handed

The parser-pack and crate-existence tables reproduce exactly. What they say
about those crates does not always survive opening them.

| claim | verdict |
|---|---|
| entl parser packs: which queries, which `parser.toml` sections | **confirmed, every cell** |
| `entl-rust-mir` — Rust via MIR | confirmed: emits `entl_semantics::SemanticObservations` |
| `entl-ts-observe` — TypeScript | confirmed as a provider, but **its only consumer is `infact/tools/ts-scoreboard`**. TypeScript observations are not wired into `infact-analysis`. |
| `entl-zig-observe` — Zig | **does not produce `SemanticObservations`.** It is a syntax walk emitting a bespoke `ContainerField` / `FieldAssignment` / `MethodCall` type. It is not the same kind of thing as the other two. |
| `entl-zig-air` — Zig via AIR | confirmed, and it is a **stdin→Parquet binary**, not a library. No consumer in these three repos; its consumer is `~/powderworks/baozi/lifetimes/`. |
| infact: Rust normalize/behaviors/effects | confirmed |
| infact: TypeScript normalize/behaviors | confirmed |
| infact: Python normalize | exists, and **nothing can reach it** — not `infact-analysis`, not `infact-cli`, not straitjacket. Reachable only from its own tests and examples. |
| infact: Zig lifetimes | same: **a leaf crate with no production consumer.** |
| "neutral crates that already exist" | incomplete. It omits **`infact-errors`**, which is the most neutral thing in the tree. |

### The two omissions that change the picture

**`infact-errors` is fully language-neutral and gated only on data.** It requires
exactly two things of a pack — `callables.scm` and `discards.scm` — and decides
which callable owns a discard by *byte-range containment*, which is a property
of trees rather than of any language:

```rust
const REQUIRED_QUERIES: [&str; 2] = ["callables", "discards"];
```

Adding error-discards for a new language costs **two `.scm` files and one
`[error-handling]` block. Zero Rust.** That is the single most important number
in this census, and the crate rename from `infact-rust-errors` recorded it
without the language table reflecting it.

**Most shipped queries have no consumer.** Grepping every `matches("…")` call in
the fleet: only `callables`, `discards`, and `behaviors` are consumed by
production code. That makes the inert set

```
rust     comments, highlights, injections
zig      folds, highlights, indents, injections, locals
```

— eight of thirteen, all dating to `init`/`wip` in git. So the brief's headline
("zig ships five queries and not one is an analysis query") is true but not
specific to zig: **rust ships three of the same kind**, and the honest framing is
that entl's packs carry a vendored editor-affordance surface alongside the
analysis surface. Those files are not free-floating — `ParserRuntime::load`
compiles every one against the pinned grammar — but nothing in this fleet reads
their results.

---

## The gaps, classified

The classification is the deliverable. `missing` = it applies and nobody built
it. `not applicable` = the language does not have the thing being modelled.
`blocked` = it applies and something else must land first.

### missing

| gap | cost, measured |
|---|---|
| **`.tsx` error discards** | copy 2 `.scm` + the `[error-handling]` block. Both queries verified compiling against the tsx grammar; all three findings reproduce. Roughly an hour, and it closes a silent divergence rather than adding a feature. |
| **JavaScript error discards** | `discards.scm` copies unchanged; `callables.scm` needs a JS variant without `type_annotation`. `[error-handling]` copies from typescript. |
| **Zig error discards** | Zig has an error-returning convention (`!T`), `catch {}`, `catch unreachable`, `_ = f()` — entl's own note lists the forms. Needs `callables.scm`, `discards.scm`, `[error-handling]` with `propagation = "declared"` and `fallible-types` naming the `!` convention. The one real question is whether `!T` is expressible as a captured return type; unverified. |
| **Python error discards** | see the note below — the recorded blocker no longer holds. |
| **A checked-in TypeScript/ECMAScript fact pack** | `infact behavior library --language typescript` already exists and works. The pack was derived once (89 callables, 19 behaviors, 16 reportable) and lives in a scratch `measure/` directory rather than in the repo, so `library-behaviors` is dormant for every ECMAScript file despite the matcher covering all three extensions. |
| **Reaching `infact-python-normalize`** | the crate is the most thoroughly measured frontend in the tree (0.086% opacity over 7,680 files) and no caller exists. It needs either a behaviors frontend on top or a route through `infact-analysis`. |
| **Reaching `infact-zig-lifetimes`** | 41–47% coverage at ~87% precision against Bun's own 2,252-field classification, and no consumer. Its natural consumer is a porting harness, which lives outside these repos. |
| **Wiring `entl-ts-observe` into `infact-analysis`** | `analyze_repository_with_observations` takes `&[SemanticObservations]` and consumes them only through `infact-rust-effects`. The TypeScript provider emits the same schema and is read only by a scoreboard tool. |
| **straitjacket's discard prose is Rust-only** | measured on the fixture: a TypeScript finding reads *"the `Err(_)` arm reads nothing"*, *"returns `Result`"*, *"propagate with `?`"*. TypeScript has none of those. `infact-core` keeps `DiscardForm` neutral and `straitjacket/src/rules/error_discard.rs` renders it in Rust vocabulary. A boundary is being honoured on one side and dropped on the other. |

### not applicable

| gap | why |
|---|---|
| pointer-ownership analysis for Rust | the borrow checker already decides it; there is no undecidable spelling to classify. *(given in the brief, confirmed)* |
| pointer-ownership analysis for TS / JS / Python | all three are garbage-collected and have no pointer whose ownership syntax leaves open. |
| `[tests]` for TypeScript | already argued in `typescript/parser.toml`: TypeScript marks tests by filename convention or a `describe`/`it` call, not by an annotation on the item, and `TestManifest` matches marker substrings against captured annotation text. An empty section would claim the same thing less clearly. **Note this is n/a to the manifest as it stands, not to the capability** — a filename-convention marker concept would make it expressible. |
| an editor-affordance surface for the analysis pipeline | `folds`/`highlights`/`indents`/`locals`/`injections` are not analysis queries and adding them to more packs buys the fleet nothing. Rust and zig having them is history, not design. |

### blocked

| gap | blocker |
|---|---|
| **call effects for TypeScript / Python / Zig** | the effects seed set comes from a call-effect catalog, and the only catalog builder is `infact-catalog`, which reads **rustdoc JSON**. There is no equivalent generator for npm, PyPI or Zig. Blocked on an ecosystem catalog source, not on `infact-rust-effects` being Rust-shaped. |
| **library behaviors for Python** | blocked on `infact-python-behaviors`, which is blocked on the same thing TypeScript hit and solved: a source of implementations to derive from. Python's stdlib *is* readable Python, so unlike TypeScript this one has an obvious corpus. |
| **library behaviors for Zig** | blocked on `infact-zig-normalize`. No Zig frontend onto `infact-normalize` exists; `entl-zig-observe` is a different kind of thing and does not produce a `Form`. |
| **Rust `.tsx`-class silent gaps generally** | blocked on nothing technical — but see the open question below about whether a pack that describes *some* of a language's forms should be able to stay silent. |

### The Python error-handling call, where a note has gone stale

`entl/notes/todo.txt` says, of the Python pack:

> The pack ships no `[error-handling]`, no `[tests]`, and no queries. All three
> are deliberate: every field of `ErrorHandlingManifest` is shaped around a
> fallible RETURN TYPE, and Python spells failure with exceptions.

**That reason no longer holds.** `Propagation::Unchecked` was added to
`ErrorHandlingManifest` on 2026-08-03 (`316b49d`) — the same day the Python pack
landed (`6786685`) — precisely for a language where any callable can raise
whatever its signature says. Its doc comment describes Python as exactly as well
as it describes TypeScript:

> Rust declares failure in the return type […] TypeScript does not: any callable
> can throw, so not catching is always available and no signature declines it.

So `propagation = "unchecked"` with empty type lists is a complete and honest
Python manifest today, and the work is writing `discards.scm` — `except: pass`,
a bare `except`, `contextlib.suppress`, `.get(k, default)` — not unblocking the
manifest. Recorded rather than acted on, because the note is required reading
and disagreeing with it should be visible.

---

## What a new language costs, measured on the two most recent frontends

| crate | src | tests | what it is |
|---|---|---|---|
| `infact-rust-normalize` | 1,003 | 272 | frontend onto `infact-normalize` |
| `infact-ts-normalize` | 1,591 | 546 | frontend, plus named arrow functions, counted `while`, multi-declarator |
| `infact-python-normalize` | 2,081 | 1,294 | frontend, plus `fuse_container_fills`, slices, qualification |
| `infact-normalize` | 3,453 | — | the neutral core and the laws |
| `infact-behaviors` | 519 | — | the neutral derivation walk |
| `infact-rust-behaviors` | 1,970 | 2,144 | Rust frontend + macro derivation |
| `infact-ts-behaviors` | 1,176 | 315 | ECMAScript frontend |
| `infact-errors` | 764 | 349 | **neutral; per-language cost is `.scm` only** |
| `infact-duplication` | 1,175 | 144 | **neutral; per-language cost is `[tokenization]` only** |

So the cost of a language is a step function, not a line:

```
tokenization only        a [tokenization] block          -> clones, text rules
+ two .scm files         + an [error-handling] block     -> error discards
+ a normalize frontend   1,000-2,100 lines               -> Form, cross-language equivalence
+ a behaviors frontend   1,200-2,000 lines + a pack      -> library reuse
+ an ecosystem catalog   does not exist for any non-Rust -> effects
```

Everything above the second row is where the money is, and the first two rows
are where the current gaps are.

---

## On abstraction — one thing already extracted that has not earned it yet

`infact-flow` (525 lines) was extracted from `infact-rust-effects` on the
argument, in its own commit message and doc comment, that two analyses need it:

> Effects ask which callables reach an allocator, over a graph of calls.
> Ownership asks where a field's value came from, over a graph of assignments.

**Measured: it has one consumer.** `grep` across both repos finds
`infact_flow` only in `infact-rust-effects`. `infact-zig-lifetimes` — the
ownership analysis named — depends on `entl-zig-observe` and nothing else, and
does no propagation at all: `classify` and `classify_with_evidence` are decision
lists over one field's own evidence, with no graph, no seeds and no closure.

This is not an argument to delete it. The extraction was clean, it named the
consumer it was anticipating, and `infact-rust-effects`' tests passed unchanged,
which is the right proof. But it is the same shape as the shelved
`infact-rust-callgraph` crate — a crate justified by a second consumer — and the
recorded rule from that episode is *two is usually not enough*. Here the second
consumer exists in the tree and does not consume it.

The honest reading: `infact-flow` is a **prediction**, and the way to settle it
is the thing `infact-zig-lifetimes` itself says it needs — "separating 'this was
allocated here' from 'this container frees it' needs the free site too, which is
a call-graph question". If ownership ever grows that pass, it will want exactly
`Graph::propagate` + `witness`. If it never does, `infact-flow` should fold back.

Nothing else in this census is a candidate for extraction. `infact-errors`,
`infact-behaviors`, `infact-normalize` and `infact-duplication` are all neutral
cores with real per-language frontends stacked on them, and each has at least
two live consumers.
