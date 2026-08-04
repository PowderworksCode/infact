You are the code-reduction session. Read this in full before doing anything.

## Your workspace, and the isolation rule

You work in ~/powderworks-slim, which holds its own clones of entl, infact, and
straitjacket as SIBLINGS. They are path dependencies (straitjacket -> infact ->
entl); the sibling layout is required or nothing builds. All three are clean, at
entl 7400b8a, infact 2798037, straitjacket 5925846.

DO NOT TOUCH ~/powderworks, ~/powderworks-ts, or any other tree. Other Claude
sessions are working in those, some with uncommitted changes. That separation is
the entire reason this checkout exists.

## The task

**Figure out how to reduce the amount of code in infact.**

The user's framing, verbatim: *five crazy plans, new query engines, new deps,
whatever it takes.*

Take that literally. Five plans, and they must differ in KIND, not in degree —
five variations on "extract some helpers and tidy up" is a failed answer. Things
explicitly on the table: replacing the execution engine, adopting new
dependencies, deleting whole crates, changing the fact model, generating code
instead of writing it, moving logic into data (queries, tables, packs), pushing
work into a different language or tool, and collapsing abstractions that earn
less than they cost.

You are NOT being asked to implement any of it yet. You are being asked to find
the five best ideas and cost them honestly.

## The baseline, measured before you launched

    infact/crates    18,187 lines across 79 files, 13 crates
                     15,866 lines of src across 55 files
                      2,321 lines of tests
    transitive dependencies    555

Per crate, largest first:

    3953  infact-rust-behaviors        954  infact-rust-errors
    2701  infact-rust-effects          545  infact-core
    2412  infact-normalize             478  infact-analysis
    2015  infact-fact-pack             440  infact-fact-registry
    1487  infact-cli                   439  infact-catalog
    1400  infact-duplication            90  infact-fact-builder
    1273  infact-rust-normalize

Largest files:

    1849  infact-normalize/src/lib.rs        <- OVER straitjacket's 1500 limit
    1500  infact-rust-effects/src/lib.rs     <- EXACTLY at it; next line trips
    1304  infact-cli/src/main.rs
    1001  infact-rust-normalize/src/lib.rs
     878  infact-fact-pack/src/lib.rs

Verify these yourself. They were taken with find/wc and include no judgment about
what the lines are worth.

## One lead I already measured — take it or reject it with a reason

DBSP is infact's declared execution engine. It is imported by **2 of 13 crates**,
in exactly two files:

    infact-rust-effects/src/lib.rs      ~26 lines touch dbsp symbols, of 1500
    infact-duplication/src/engine.rs    ~17 lines touch dbsp symbols, of 627

and it accounts for **321 of the 555 transitive dependencies** — 58% of the
dependency graph.

Do not leap from that to "delete DBSP". The honest counter is that one of those
call sites is `circuit.recursive(...)` at infact-rust-effects/src/lib.rs:1115,
building an incremental fixed point over `Stream<_, OrdZSet<EffectRelation>>`.
Incremental transitive closure is the genuinely hard thing DBSP provides, and
reimplementing it badly would be a large step backwards for a dependency count
that nobody pays at runtime. The real question is whether the incrementality is
load-bearing for how infact is actually used, or whether it is paying for a
property no consumer exercises. **Measure that before recommending either way.**

Note also that infact's AGENTS.md makes DBSP a boundary condition: "Public facts
must not expose DBSP streams, batches, weights, or circuit types." That the
engine is already hidden behind a seam is what makes replacing it thinkable.

## Required reading before you propose anything

1. infact/AGENTS.md and entl/AGENTS.md — the boundaries. Several of them exist to
   stop exactly the kind of collapse you might propose. You may propose crossing
   one; you must say so explicitly and argue it.
2. infact/notes/todo.txt — all of it. Much of this code was built deliberately
   and the note records the reasoning and what was measured. Proposing to delete
   something the note already justifies, without engaging the justification, is
   the main way this task goes wrong.
3. entl/notes/todo.txt — in particular "Parser-pack queries for the
   discarded-error check". That is already a live plan of exactly the shape you
   are looking for: moving ~210 of infact-rust-errors' 744 lines of recognition
   OUT of Rust and into `queries/discards.scm` tree-sitter queries, turning code
   into data. The loader and provenance digest are DONE; writing the queries is
   what remains. Evaluate whether that generalizes — infact-rust-effects and
   infact-rust-behaviors also do node-kind recognition in Rust.
4. straitjacket/notes/todo.txt — straitjacket is the consumer and owns the
   file-size rule that two files are at or over.

## What each of the five plans must state

- **What it deletes**, in measured lines and named files or crates. Not "would
  simplify X" — a number you counted.
- **What it adds**: new dependencies, new generated artifacts, new concepts a
  reader has to learn. A plan that removes 2,000 lines of Rust by adding a
  dependency with its own 50,000 has not obviously won; say which way it goes.
- **What it breaks**, including which AGENTS.md boundary it crosses, if any.
- **What it makes impossible or harder later.** Every collapse forecloses
  something. Name it.
- **The cheapest experiment that would falsify it**, per the notes' own repeated
  lesson: measure a hypothesis on one case before building for it. That note
  records four consecutive mispredictions where the corpus disagreed with the
  reasoning, so treat your own confidence as weak evidence.

Rank them at the end, and say plainly which one you would actually do first and
which of the five you think is probably wrong but worth writing down anyway. The
user asked for crazy on purpose — do not self-censor to a safe list, but do not
dress a safe list up as a bold one either.

## Constraints that still bind

- Do not commit, push, or publish unless asked. Both AGENTS.md say so.
- **Do NOT prefix builds with `RUSTC_WRAPPER=`.** sccache IS installed and
  configured as the global rustc-wrapper on this box, so AGENTS.md's "if a
  compiler cache is unavailable" caveat does not apply. Unsetting it disables
  the cache. Incremental compilation is now disabled globally in
  ~/.cargo/config.toml because sccache hard-errors when it is on. Just run
  `cargo ...` plainly. Prefer `-j 4`; other sessions share this box.
- rustc 1.96.1 is the default and is correct. Do NOT use 1.97.1 — it ICEs on
  dbsp. entl-rust-mir pins nightly-2026-07-18 and is excluded from the workspace.

## First move

Read, measure, and come back with the five plans. Do not start implementing.
If the required reading contradicts anything in this brief, the notes win — say
so when it happens.
