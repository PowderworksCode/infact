# Can a `Form` be turned back into code? Phase 0, measured

Answer: **no, and the reason is structural rather than fixable.** 4 of 120 real
functions survive a Rust → `Form` → Rust round trip, and the four that survive
share a property that excludes almost everything a port has to translate.

Everything below is reproducible from this tree. The instruments are
`crates/infact-rust-normalize/examples/census.rs` and `examples/lower.rs`.

## The bar

Stated before measuring, per the brief: **the output compiles, and the crate's
own tests still pass.** Nothing softer counts, and one thing softer had to be
thrown out — see "a vacuous pass" below.

Method: lift a function to `Form`, lower it back, splice the result into the
original source in place of the original body, and build. The signature,
imports, types and every other function stay exactly as written. Only what was
lifted is replaced by what came back, so anything that fails, fails because the
form did not keep it.

Target crate `infact-normalize`, 120 functions, 27 tests. Chosen because it is
self-contained, so a failure is never a missing dependency.

## The numbers

| | compiles | compiles **and** tests pass |
|---|---:|---:|
| from `Form` alone | 15 / 120 (12.5%) | **0 / 120** |
| from `Form` + a name side-table | 20 / 120 (16.7%) | **5 / 120**, of which 4 are real |

The name side-table is the brief's first candidate shape: `Roles` already knows
every identifier as it walks, so recording them beside the form costs nothing
and changes no form. It is now `Roles::ledger()` and
`NormalizedFunction::names`. Matching does not consult it, and cannot: a form
that carried names would stop comparing two implementations that differ only in
what their authors called things, which is the entire point of the form.

**It bought five compiles and four correct functions.** That is the measurement
that decides the architecture, and it is worth being precise about why so
little: names were never the hard part.

### A vacuous pass

`arm_pattern` cleared the strict bar and is not a translation. Both its
`matches!` calls lowered to `todo!()`, which has type `!` and so type-checks
anywhere. Replacing its whole body with `panic!()` also passes all 27 tests —
it is never exercised. The bar as stated admits a body that is uniformly
`todo!()` wherever coverage is thin, so a project's own tests are a floor on
correctness, not a proof of it. Genuine round trips: **4 of 120, 3.3%.**

### What the four have in common

`bind`, `bind_anonymous`, `collect_bindings`, `find`. Straight-line statements,
method calls, and `for` loops. No construction, no macro, no `match`, no `?`,
no or-pattern, no `let mut`, and no borrow in a position the type checker looks
at. That is the whole of the mechanically translatable category.

24.4% of 550 production functions are free of construction, macros and
decisions — the ceiling. Roughly one in six of those actually round-trips.

## What was lost, in rustc's own words

Error codes across the failures, with the name side-table in place:

| code | functions | what it is |
|---|---:|---|
| E0599 | 52 | no such method — peeled adapters, and `T::new()` guessed for a discarded constructor |
| E0531 | 20 | no such tuple struct — `Self::Binding` lowered to `Binding` |
| E0433 | 16 | undeclared type — the same stripped path qualification |
| E0425 | 14 | unresolved name — what the side-table did not reach |
| E0308 | 8 | type mismatch — erased borrows and casts |
| E0061 | 3 | wrong arity — a constructor's arguments were discarded |

Without the side-table E0425 was 48. Adding names moved it to 14 and moved
nothing else, because it let rustc get far enough to report the real problem.
**The residue is method and path resolution, and no table of names touches it.**

### The destructive rules, each deliberate

Every one of these is a decision recorded in the doc comments, made for
matching, and each is fatal for emission:

- **`Construct(String)`** — `HashMap::with_capacity(8)` and `Renaming::default()`
  both become `Construct("HashMap")` / `Construct("Renaming")`. The constructing
  function and every argument are gone. 46.7% of production functions contain
  one. `todo.txt` already records this as an open matching bug under
  "THE CONSTRUCTOR'S OWN IDENTITY IS DISCARDED"; it is also the single largest
  cause of lowering failure.
- **`reference_expression` in `unwrap_noise`** — `&x`, `&mut x` and `x` are one
  form. `pub fn manifests(&self) -> &[FactPackManifest] { &self.manifests }`
  lowers to `self.manifests` and does not compile. **This is the Zig pointer
  problem exactly**, in the language the lift already handles: the borrow/own
  distinction that `LIFETIMES.tsv` exists to classify for Zig is destroyed by
  the Rust lift too.
- **`SEQUENCE_ADAPTERS` / `VALUE_CONVERSIONS`** — `.iter()`, `.into_iter()`,
  `.clone()`, `.into()` peeled from any receiver chain. A `Transform` lowers to
  `.map()` on something that is not an iterator.
- **`variant_name`** — `rsplit("::")`, so `Self::Binding` becomes `Binding`.
  This is what E0531 and E0433 are, and between them they touch 36 of the 100
  failures (some functions hit both, so the union is smaller).
- **`Let`** carries no mutability and no type annotation. 1,058 sites.
- **`Select`** holds its arms sorted, so written order is gone; and any `match`
  with a bare `_` arm does not become a `Select` at all — `_` is anonymous in
  the grammar, so `named_children` is empty and the whole `match` falls to
  `Opaque`. 135 such matches in the corpus.
- **Or-patterns** are `Pattern::Ignored`, so `Self::Tuple(parts) | Self::Variant { parts, .. }`
  binds nothing and the body's `parts` resolves to a *free variable*. The
  binding structure is wrong, not merely unnamed.
- **`Opaque` is the largest single node kind at 18.4%** of 23,286 nodes, and
  `token_tree` is 2,681 of them. A macro's arguments are unstructured; 28.6% of
  production functions contain one.

## Two findings that are about infact, not about lowering

**1. `Roles` has no scoping, and a `Select` arm can name another arm's
binding.** `Pattern::size` lowers to a `Variant` arm whose body references
`v0`, bound by the `Tuple` arm before it. Matching has never had to care.
4 of 550 functions (0.7%) do this. It is worth knowing because the failure mode
is not a compile error — it is a form that claims a data dependency the source
does not have, which is the same shape as the eight bugs `collisions.rs` was
built to catch. Detector: `leaks_scope` in `examples/lower.rs`.

**2. 5.3% of production functions share a form with a differently-written
function.** 29 of 548, in 8 classes, on infact's own source with test fixtures
excluded. This is a floor on unrecoverability that needs no emitter to
establish: no function can return two answers. Two of the classes are genuine
near-duplicates worth looking at on their own account —
`ambiguity.rs:194:collect` and `coverage.rs:129:collect` are the same 46-node
function, as are `lib.rs:690:repository_module` and `errors.rs:63:repository_module`.

## Which of the four shapes is right

The brief listed four. The evidence picks the last one.

- **`Form` + a side-table of names.** Built, measured: 4 of 120. Names were not
  the bottleneck.
- **`Form` + a side-table keyed by span.** The table would have to carry
  constructor identity and arguments, path qualification, borrows, mutability,
  type annotations, peeled adapters, macro token trees, arm order and
  or-patterns. That list *is* the source. A table large enough to lower makes
  the form contribute nothing to lowering.
- **Extend `Form` with what lowering needs and matching ignores.** Every item on
  that list is a distinction normalization exists to destroy. Recording whether
  a receiver was `.iter()` or `.into_iter()` un-normalizes the very equality the
  crate is built on — a loop and a combinator agreeing is the central claim, and
  `SEQUENCE_ADAPTERS` is what makes it true. Note that `collisions.rs` would not
  catch this: it asserts distinct callables derive *distinct* forms, so it
  guards against erasing too much and is silent about keeping too much. A change
  that broke matching this way would pass it.
- **Lowering does not belong on `Form`.** This is the conclusion.

There is a more basic reason, which no amount of side-table fixes: **`Form` is a
function-body IR.** `normalize_file` collects `function_item` and nothing else.
There is no struct, enum, trait, impl, `use`, module, generic parameter, where
clause, or return type anywhere in it. A port needs all of those, and the form
has never seen one.

## What this means for Zig → Rust, and for cowbird's estimate

**The Zig lift does not exist**, so nothing here is expressible for Zig yet.
`infact-python-normalize` is the most recent evidence of that cost: 1,600 lines,
and `todo.txt` records it finding two bugs in the shared core on the way.

But building it would not help, and the reason is in baozi rather than here.
`PORTING.md` says every other module's **signatures already exist** — "types,
fields, enums, constants and function signatures, ported and fixed" — and are
handed to the model as context. So the declaration layer, which is the part a
mechanical translator could plausibly do well, is already handled by other
means and needs no `Form`. What the model is actually paid for is **bodies**,
which is precisely what the round trip above destroys.

And the saving would be small even if it worked. `COST.md` measures the real
rewrite at **~$165,000** of API spend against a pure-token floor of **~$179** —
tokens are **0.11%** of what was paid. The other 99.89% is "retries, reasoning,
iteration, and the 6,778 commits it took to converge". Mechanically translating
a declaration removes its tokens; it does not remove the iteration, which is
where the money is. The brief's first premise — that every declaration a machine
translates is one nobody pays a model to translate — is true and worth roughly a
tenth of a percent.

**The two assets remain valuable and are unaffected by this result.**
`MAPPINGS.md` (88 namespaces, 12,582 call sites) and `LIFETIMES.tsv` (2,252
classified pointer fields) are inputs to the *model's* context, which is how
`PORTING.md` already uses them. They do not need a supercompiler to pay off, and
routing them through one would strip exactly what makes them useful — the Rust
lift erases `&` versus `&mut` as noise, which is the distinction `LIFETIMES.tsv`
was built to record.

## Where the boundary is, since an average would hide it

Not comptime. Comptime is beyond the wall, but the wall is much closer in:

- **construction** — any `T::new(..)`, struct literal, or `Default::default()`
- **any borrow whose mutability or existence the type checker checks**
- **any macro**
- **any `match` with a `_` arm, or any arm with a guard or an or-pattern**

Those four exclude ~76% of infact's own Rust before allocators, error unions or
comptime are reached. A Zig corpus would be worse, not better.

## Reproducing

```
cargo run --example census -p infact-rust-normalize -- crates/*/src
cargo run --example lower  -p infact-rust-normalize -- crates/*/src
cargo run --example lower  -p infact-rust-normalize -- --show <path>
LOWER_ONLY=<fn> cargo run --example lower -p infact-rust-normalize -- <src> --splice-into <dir>
```

## Caveats, stated rather than buried

- One corpus: infact's own Rust. It is idiomatic modern Rust and may be harder
  than average (heavy `match`, heavy macro use in tests) or easier (little
  unsafe, no async in the measured crate).
- One crate for the compile measurement, 120 functions. The census and collision
  numbers cover all 550.
- The emitter is deliberately the most generous one the form supports, and every
  guess it makes is counted. It could be improved at the margins — reinstating
  `&` by type inference, say — but not past E0599 and E0531, which are 72 of the
  100 failures and are missing information rather than missing effort.
