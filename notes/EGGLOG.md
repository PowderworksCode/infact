# The egglog question, settled — and what settling it uncovered

*Session write-up, 2026-08-13, branch `egglog-evaluation`. Line references are
to 482bfdd. The prototypes named here (two e-graph ports, a law checker, a
frame-driven normalizer) lived in a session scratch directory; their
load-bearing data — the frame files and idiom queries — is reproduced in the
appendices, and every number below states how it was obtained.*

The question was: should infact be rebuilt on egglog? **No.** The evaluation
that produced that answer also produced three things worth more than the
verdict: a semantic verification of `simplify`'s laws (one real soundness
finding), the theory each hand-built piece turns out to be an instance of, and
a measured demonstration that ~90% of every per-language normalizer is
declarative data — which reframes where the fleet's normalization cost goes
next.

---

## 1. The e-graph verdict

*Measured, two prototypes: egglog 2.0.0 and egg 0.11.0, each porting laws
from `simplify.rs` and running them against the real crate's output.*

### The normalizer is a deterministic pass, by design

`Form::simplify` (`crates/infact-normalize/src/simplify.rs:58`) rewrites to a
bounded fixpoint: `MAX_SWEEPS = 8` (line 33), `MAX_UNFOLDS = 64` (line 54),
and a priority-ordered `or_else` chain (lines 165-177) in which the first law
to fire wins. Every choice a rewriting system normally leaves open — rule
order, traversal order, stopping — is frozen. That is not an accident to be
fixed by an e-graph; it is the design. Consumers only ever compare
`simplify(A)` against `simplify(B)` where both sides ran the identical
function (`infact-rust-behaviors/src/lib.rs:199-201`), so the frozen choices
need to be reproducible, not provably confluent — agreement by shared code,
substituting for confluence by proof. Residual divergence is absorbed by the
matcher's deliberate relaxations (`contains_fused`,
`infact-rust-behaviors/src/lib.rs:213-219`; `without_result_reference`,
`crates/infact-normalize/src/lib.rs:724-731`), not by exploring alternatives.

### egglog: rules 30% shorter, everything else ~10× longer

Porting the two purely structural laws (`as_escape`, `as_searched`; 51 lines
of Rust including dispatch) cost 243 lines: 30 of rules, 40 of schema for
`Form`'s 26 variants, 170 of bidirectional conversion, 3 of build workaround
(`egglog-add-primitive` 2.0.0 needs `syn` `features=["full"]`, which a fresh
downstream crate only gets under resolver v1). Two findings matter more than
the line count:

- **Every rule needed `(subsume ...)` on its left-hand side.** Extraction
  minimizes term size, and infact's normal forms are *larger* than their
  inputs on purpose (`simplify.rs:44-48`). With plain `union`, extraction
  returned the un-rewritten input. Subsuming every LHS is a directed rewrite
  system paying e-graph overhead — the prototype reproduces infact's behavior
  only by turning the equality saturation off.
- `is_break` — match any variant whose last `::` segment is `Break`
  (`simplify.rs:634-637`) — is an unbounded operator set, inexpressible as a
  rule; it became a Rust primitive registered on the e-graph.

Performance, release build, one law, measured across form sizes: ~1 ms fixed
cost per form, amortizing to 24× slower at 73 nodes, 10× at 289, 6× at 1,153.
Real but not disqualifying; the architecture arguments are what disqualify.

### egg: cheaper, and blocked structurally

48 packages against egglog's 159, no build workaround, and the whole port was
41 lines — but `define_language!` cannot express a variant carrying a string
*and* children (`macros.rs:187` takes exactly one child type), which is 9 of
`Form`'s 26 variants; the documented fallback bakes the name into the
operator, making `is_break` unwritable as a pattern at all. And egg has no
subsume, so direction must be a cost function — which provably cannot order
`(return (do A B))` against `(do A (return B))`: identical node multisets,
cost 6 either way, for every node-local cost function. Rearrangement laws are
invisible to cost.

### Why the verdict is structural, not incidental

Infact's most valuable steps are deliberately **not** equivalences:
`generalized()` promotes unbound names to holes (`simplify.rs:81`) — a
widening; `lifted_from_one_step` opens with "THIS IS NOT A LAW"
(`simplify.rs:666`) and is applied only under caller context
(`crates/infact-behaviors/src/lib.rs:277`). A union-find can only hold "these
ARE equal"; infact's value comes from controlled claims of the form "these
are not equal but should match" — which must live in a matcher, quarantined
and directional. Additionally, pack contents are digested — the `Direction`
field is `skip_serializing_if` precisely because a serialized default "would
change the digest of every published behavior"
(`crates/infact-normalize/src/lib.rs:195-201`) — so the normal form is a
content address, and determinism is load-bearing for the pack format.

The Datalog half of the pitch dissolves on inspection: the workspace's entire
ascent program is one transitive-closure rule
(`crates/infact-flow/src/lib.rs:87-96`). There is nothing for egglog's
Datalog to subsume.

In the literature's terms: infact is *canonicalize-then-match* (compiler
canonicalization + one-way matching); the alternative is
*saturate-then-E-match* (equality saturation + E-matching). The nearest
published sibling — Yogo, "Semantic Code Search via Equational Reasoning,"
PLDI 2020, built on Cubix — chose saturation, and had to: it matches foreign
code against a curated rule database, so it cannot run one normalizer over
both sides. Infact derives both sides itself, which is exactly what makes the
cheaper model available.

---

## 2. The laws, checked against a reference interpreter

*Measured: a ~470-line reference interpreter defining what `Form` means
(escapes propagate to function boundaries; lambdas are boundaries; iteration
visits are logged), driving the real `simplify()` through a path dependency.
400 random cases per law from a seeded generator; every case also checks
`simplify(simplify(x)) == simplify(x)`.*

| law | verdict |
|---|---|
| `as_fused` | holds, exact (fold fusion) |
| `as_searched(.first/.last)` ∘ `as_optional_search` | holds, exact |
| `as_returned_sequence` | holds, exact |
| `as_escape` ∘ `as_traversal` | holds **modulo the Break/Continue coercion** — the crate's own claim, made precise |
| `as_recovered_escape` ∘ both (the full `find` derivation) | holds, exact — `break_value` *is* the coercion |
| `as_unfolded`, pure bodies | holds, exact |
| `as_unfolded`, body with mid-body `return` | **FAILS** |
| `lifted_from_one_step` | confirmed a non-law, as documented — values and traces both differ |

Idempotence: zero violations.

### The finding

`as_unfolded` inlines a lambda body containing a non-tail `Return` across the
function boundary that gave the `Return` its meaning:

```
let f = |x| { if x < 1 { return 42 } 7 };  let y = f(-1);  y + 100
```

Original: `f(-1)` is 42, result **142**. Simplified: the inlined `return 42`
escapes the enclosing function — result **42**. Reachable from both
frontends: TS keeps mid-body returns by design ("a `return` anywhere earlier
is an escape from the middle of the work … has to stay",
`crates/infact-ts-normalize/src/cleanup.rs:142-143`), and Rust emits `Return`
wherever it appears, closures included
(`crates/infact-rust-normalize/src/lib.rs:633-638`). The natural fix is in
`unfoldable`'s spirit: refuse bindings whose lambda body contains a non-tail
`Return`, exactly as it already refuses cycles (`simplify.rs:593`). Not
applied on this branch.

Worth noticing: seven law families are instances of named theorems and
passed; the one that failed is the one that is *not* an instance of its
theorem — β-reduction is sound only when substitution respects binding
structure, and `Return` smuggles in a second binding structure (the function
boundary) that `apply_bindings` does not track.

### The theory each piece is an instance of

*Argued, with the anchors checked against the code.*

- The iteration algebra (`Traverse`/`Transform`/`Retain`/`Sift`/`Accumulate`)
  and the fusion laws: universal property of fold (Hutton 1999;
  Bird–Meertens). `as_fused` is textbook fold fusion.
- `lifted_from_one_step`: the `Step` type from *Stream Fusion: From Lists to
  Streams to Nothing at All* (Coutts, Leshchinskiy, Stewart 2007) —
  `Done`/`Skip`/`Yield`. Rust's external iterators are stream-fusion step
  functions; the lift is un-fusing one step back to its combinator, and
  `is_stateful_step`'s refusal list (`simplify.rs:717`) matches the paper's
  boundaries.
- `as_unfolded` + fuel + `generalized()`: Burstall–Darlington unfold/fold,
  with the fuel constant standing in for positive supercompilation's whistle
  (Turchin; Sørensen–Glück–Jones).
- `Local`/`Free` renumbering: De Bruijn (1972); `canonical()` is
  α-normalization.
- The matcher: one-sided first-order matching with non-linear variables; its
  holes are the first-order shadow of Miller's higher-order patterns (1991).
- The pipeline as a whole: compiler canonicalization (MLIR's greedy rewriter
  is the same shape — priority-ordered, directional, no confluence proof),
  inside the clone-detection taxonomy (Roy & Cordy), pushing toward Type-4.

---

## 3. The frame experiment: normalizers are ~90% data

*Measured: a generic driver interpreting per-language TOML frame files
("PropBank frames for syntax": node kind → Form constructor + field-to-role
map), with idiom recognition as tree-sitter queries, differentially swept
against the real `normalize_function` of both hand normalizers.*

| per language | C | TypeScript/JS |
|---|---|---|
| corpus | 695 files — sqlite3 amalgamation, libgit2, zlib, zstd, mimalloc, liblzma, libssh2 | 1,837 files of published npm code |
| functions compared | 15,005 | 17,731 |
| frame file: frames + tables + idiom queries (data) | 128 | 191 |
| language builtins the driver needed (code) | 121 | 563 |
| hand normalizer replaced (code) | 611 | 1,226 |
| **exact agreement with production** | **100.0%** | **100.0%** |

Shared driver core: ~1,054 lines, paid once (dispatch, op interpreters, query
engine with `#eq?` predicate enforcement, cleanup passes). Sweeps run in ~5
seconds each. Frames-only (no idiom queries), C agreement was 92.9% with
every single divergence attributed to the index-walk idiom and zero frame
bugs — the frame table is exactly right everywhere it claims to apply, and
the sweep proves it function by function.

What the split looks like in practice:

- **Frames** are pure data: `binary_expression → Binary{operator, left,
  right}`. The mechanical majority of both normalizers, including pieces that
  looked semantic — `switch` → `Select` was fully frame-expressible.
- **Tables** are the name lists that were already `const` arrays in the hand
  code (identity operations, `this`-arg call shapes, `filter`→`Retain`), now
  in TOML, interpreted by shared builtins.
- **Idiom queries** carry the recognition half of the counted loops. One
  query shape serves both languages (Appendix B), with `[...]` alternation
  covering C's two counter spellings in one pattern where the hand code
  needed two match arms. What stays code is crisp: capture-text arithmetic
  (direction), the indexed-sequence search, body construction — and
  counted-`while`'s "the body advances the counter," which is a text search
  no query can express.
- Op vocabulary after two languages: 23 in C / 26 in TS, 14 shared. Python —
  the largest normalizer, 2,081 lines — is the convergence test.

### The query out-recognized production

First TS run disagreed on 94 functions, all one class, direction inverted:
the query recognized index walks the hand normalizer misses. `loop_counter`
inspects only the initializer's **first** declarator
(`descendant(initializer, "variable_declarator")`,
`crates/infact-ts-normalize/src/lib.rs`), so Babel's shipped helpers —

```js
for (var keys = Object.getOwnPropertyNames(defaults), i = 0; i < keys.length; i++)
```

— read as opaque today. The query matches any declarator and finds `i = 0`.
Kept behind a `first-declarator-only` compat flag so fidelity (100.0%) and
improvement (94 functions) are separately measurable. The C hand normalizer
has the same blindness, but the class does not occur in the C corpus — it is
a JS bundler idiom. Flipping the flag is a deliberate behavior change with a
pack-digest bump, to be taken only after the hand normalizers are gone.

### Placement (entl excluded: language data is being extracted from it)

| artifact | home | why |
|---|---|---|
| frame files, tables, idiom queries | **treebank**, beside each grammar crate | frames break exactly when grammars bump; the patch author is the frame author; the certifying sweep and corpora are already there. Frame accounting (framed / idiom-lifted / deliberately-opaque / unknown, no unknowns) becomes a second column in the existing sweep reports. |
| op vocabulary (the ~30 ops and their roleset shapes) | **langbank**, versioned | the treebank↔infact decoupler: treebank writes frames against langbank's vocabulary, infact implements it, no treebank↔infact edge is ever created. Every arrow still points down. |
| driver + language builtins | **infact** | "Infact may interpret concrete syntax trees and parser metadata" (AGENTS.md) — the driver is precisely that. Per-language crates survive, gutted to their builtins. |
| certification sweeps + corpora | **treebank** | already true. |
| semantic oracles | **propbank**, when it exists | compiler-derived desugaring facts replace the session's toy interpreter as the authority that laws and lifts preserve meaning; this is what would catch the `as_unfolded` class at corpus scale. |

Migration keeps the differential harness alive as an infact test until the
frames ship end to end; after deletion it flips to golden-corpus mode in
treebank.

### Types stay out of frames; rolesets are the extension path

*Argued.* A frame is certifiable by a parser alone — types would put a
compiler in treebank's loop, break the JS/TS form bridge (typed and untyped
code must normalize identically), and destabilize digests (type inference is
environment-dependent). The type pressure is real — `.map` receiver
ambiguity, C pointer walks — and the codebase already answers it three ways:
grade certainty instead of resolving types (`infact-rust-errors`), follow
constructed types outside the normalizer (`infact-behaviors`), verify
signatures against catalogs at match time (AGENTS.md boundary). The frame
formalism's version of that answer is PropBank's own: **rolesets** —
enumerate a shape's readings as data (the hand normalizers already do this
implicitly: subscript's three senses, `at(0)`/`at(-1)`, screaming-case),
select once via pinned evidence (parse-local → catalog digest → propbank),
default to today's reading so untyped code degrades to exactly current
behavior, and record the chosen sense in the witness. The gate for adding a
second sense to any predicate is a measured misreading rate over the corpus,
not architectural taste.

---

## 4. Housekeeping found along the way

- `README.md` still says "DBSP maintains relations incrementally"; DBSP is
  gone as of 2f97ada (`rust-toolchain.toml:3` says so). AGENTS.md is already
  correct.
- `crates/infact-rust-effects/Cargo.toml:11` declares `ascent` and never uses
  it — the crate reaches Datalog only through `infact-flow`.
- The `as_unfolded` non-tail-return guard and the `loop_counter`
  first-declarator fix are each one-guard changes plus a regression test.

---

## Appendix A — C frame file (`frames/c.toml`, 128 lines of data)

Reproduces 100.0% of `infact-c-normalize` over 15,005 functions, with the
idiom query of Appendix B.

```toml
[idiom."counted-for"]
builtin = "counted-walk-c"
first-declarator-only = true  # replicate the hand normalizer; false finds more walks
query = '''
(for_statement
  initializer: [
    (declaration (init_declarator declarator: (_) @counter value: (_) @start))
    (assignment_expression left: (_) @counter right: (_) @start)
  ]
  condition: (binary_expression left: (_) @bound operator: _ @cmp)
  update: (_) @increment
  body: (_) @body
  (#eq? @bound @counter)) @root
'''

[statement."compound_statement"]
op = "block"

[statement."expression_statement"]
op = "child-expression"

[statement."return_statement"]
op = "return"

[statement."if_statement"]
op = "branch"
condition = "condition"
consequence = "consequence"
alternative = "alternative"
alternative-unwraps = true

[statement."declaration"]
op = "declaration"

[statement."labeled_statement"]
op = "last-child"

[statement."break_statement"]
op = "opaque"
kind = "break"

[statement."continue_statement"]
op = "opaque"
kind = "continue"

[statement."goto_statement"]
op = "opaque"
kind = "goto"

[statement."do_statement"]
op = "opaque"
kind = "do"
body = "body"

[statement."while_statement"]
op = "opaque"
kind = "while"
body = "body"

[statement."for_statement"]
op = "idiom"
name = "counted-for"
kind = "for"
body = "body"

[statement."switch_statement"]
op = "select"
scrutinee = "condition"
body = "body"
arm-kind = "case_statement"
arm-value = "value"
default-name = "default"

[statement."comment"]
op = "skip"

[statement."preproc_call"]
op = "skip"

[expression."identifier"]
op = "resolve"
constants = { NULL = "NULL" }

[expression."number_literal"]
op = "number"

[expression."string_literal"]
op = "constant"

[expression."char_literal"]
op = "constant"

[expression."concatenated_string"]
op = "constant"

[expression."true"]
op = "constant"

[expression."false"]
op = "constant"

[expression."null"]
op = "constant-named"
value = "NULL"

[expression."call_expression"]
op = "call"
callee = "function"
arguments = "arguments"

[expression."field_expression"]
op = "field"
value = "argument"
name = "field"

[expression."subscript_expression"]
op = "subscript-walk"
name = "at"
receiver = "argument"
field = "index"

[expression."assignment_expression"]
op = "assign"
operator = "operator"
target = "left"
value = "right"

[expression."binary_expression"]
op = "binary"
operator = "operator"
left = "left"
right = "right"

[expression."update_expression"]
op = "counted-update"
argument = "argument"

[expression."conditional_expression"]
op = "branch"
condition = "condition"
consequence = "consequence"
alternative = "alternative"

[expression."pointer_expression"]
op = "unwrap-child"

[expression."parenthesized_expression"]
op = "unwrap-child"

[expression."cast_expression"]
op = "unwrap-field"
field = "value"

[expression."unary_expression"]
op = "operator-opaque"
operator = "operator"
argument = "argument"

[expression."comma_expression"]
op = "child-sequence"

[expression."sizeof_expression"]
op = "opaque"
kind = "sizeof"
```

## Appendix B — TypeScript/JS frame file (`frames/ts.toml`, 191 lines of data)

Reproduces 100.0% of `infact-ts-normalize` over 17,731 functions, cleanup
passes and call-convention layer included.

```toml
[language]
block-cleanup = true          # alias inlining + dead-binding dropping per block
empty-block = "sequence"
statement-fallback = "expression"
opaque-children = true
valued-bodies = true          # a body's trailing return is what it is worth
declare-this = true

[tables]
identity-operations = [
  "ToObject", "ToLength", "ToInteger", "ToIntegerOrInfinity", "ToNumber",
  "ToUint32", "ToPropertyKey", "RequireObjectCoercible", "IndexedObject",
]
this-arg-calls = ["callContentFunction", "callFunction", "call"]
precondition-operations = [
  "IsCallable", "IsConstructor", "IsNullOrUndefined", "ArgumentsLength",
  "GetArgument", "DecompileArg", "IsPackedArray", "IsObject",
]
throwing-operations = ["ThrowTypeError", "ThrowRangeError", "ThrowInternalError"]
sequence-adapters = ["slice", "values", "entries", "flat", "at"]

[tables.sequence-operations]
filter = "retain"
map = "transform"
forEach = "traverse"
flatMap = "sift"

[tables.sequence-ends]
shift = "first"
pop = "last"

[idiom."counted-for"]
builtin = "counted-walk"
first-declarator-only = true  # replicate the hand normalizer; false finds more walks
query = '''
(for_statement
  initializer: (_ (variable_declarator
    name: (identifier) @counter
    value: (_) @start))
  condition: [
    (expression_statement (binary_expression left: (identifier) @bound operator: _ @cmp))
    (binary_expression left: (identifier) @bound operator: _ @cmp)
  ]
  increment: (_) @increment
  body: (_) @body
  (#eq? @bound @counter)) @root
'''

[statement."comment"]
op = "skip"

[statement."empty_statement"]
op = "skip"

[statement."throw_statement"]
op = "skip"

[statement."expression_statement"]
op = "child-expression"
skip-throwing-calls = true

[statement."return_statement"]
op = "return"

[statement."statement_block"]
op = "block"

[statement."if_statement"]
op = "branch"
condition = "condition"
consequence = "consequence"
alternative = "alternative"
alternative-unwraps = true
drop-only-throws = true       # a guard that only throws is a precondition
presence-test-unwraps = true  # `if (k in O)` inside a walk asks nothing new

[statement."for_statement"]
op = "idiom"
name = "counted-for"
kind = "for"
body = "body"
literal-when-empty = true

[statement."for_in_statement"]
op = "traverse-of"
right = "right"
left = "left"
body = "body"

[statement."while_statement"]
op = "counted-while"          # body-text search: not expressible as a query

[statement."variable_declaration"]
op = "declaration-patterns"

[statement."lexical_declaration"]
op = "declaration-patterns"

[statement."switch_statement"]
op = "select"
scrutinee = "value"
body = "body"
arm-value = "value"
default-ignored = true

[statement."break_statement"]
op = "variant"
name = "Break"

[statement."continue_statement"]
op = "variant"
name = "Continue"

[expression."parenthesized_expression"]
op = "unwrap-child"

[expression."as_expression"]
op = "unwrap-child"

[expression."satisfies_expression"]
op = "unwrap-child"

[expression."non_null_expression"]
op = "unwrap-child"

[expression."type_assertion"]
op = "unwrap-child"

[expression."update_expression"]
op = "unwrap-child"

[expression."await_expression"]
op = "unwrap-child"

[expression."spread_element"]
op = "unwrap-child"

[expression."identifier"]
op = "resolve"
screaming-case-paths = true

[expression."shorthand_property_identifier"]
op = "resolve"
screaming-case-paths = true

[expression."this"]
op = "resolve-named"
name = "this"

[expression."number"]
op = "number"

[expression."true"]
op = "constant"

[expression."false"]
op = "constant"

[expression."null"]
op = "constant"

[expression."undefined"]
op = "variant"
name = "None"

[expression."string"]
op = "constant"

[expression."template_string"]
op = "constant"

[expression."regex"]
op = "construct"
name = "RegExp"

[expression."array"]
op = "construct"
name = "Array"
spread-copy = true            # [...xs] is a copy of xs, and a copy is not behavior

[expression."object"]
op = "construct"
name = "Object"

[expression."unary_expression"]
op = "unary-js"
operator = "operator"
argument = "argument"

[expression."binary_expression"]
op = "binary"
operator = "operator"
left = "left"
right = "right"
operator-map = { "===" = "==", "!==" = "!=" }

[expression."ternary_expression"]
op = "branch"
condition = "condition"
consequence = "consequence"
alternative = "alternative"

[expression."call_expression"]
op = "call-js"
callee = "function"
arguments = "arguments"

[expression."new_expression"]
op = "construct-from-field"
field = "constructor"

[expression."member_expression"]
op = "field"
value = "object"
name = "property"

[expression."subscript_expression"]
op = "subscript-first-or-field"
value = "object"
field = "index"

[expression."arrow_function"]
op = "lambda"

[expression."function_expression"]
op = "lambda"

[expression."function_declaration"]
op = "lambda"

[expression."assignment_expression"]
op = "assign"
operator = "operator"
target = "left"
right = "right"

[expression."augmented_assignment_expression"]
op = "assign"
operator = "operator"
target = "left"
right = "right"

[expression."sequence_expression"]
op = "child-sequence"
```
