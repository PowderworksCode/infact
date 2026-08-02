# Measurement harnesses

Neither of these is part of the build. They are how the behavior work is
measured, and both were reconstructed from memory more than once before being
put here.

## clippy-scoreboard.py

Scores our findings against clippy's own ui tests, which carry their ground
truth inline as `//~^ lint_name` annotations — a labelled corpus nobody wrote
for us.

A finding is credited only when it covers an annotated line **and** names an API
that lint is about. Requiring only overlap is not a metric: an earlier version
scored `manual_flatten` at 10/10 on the strength of `Option::is_none_or`
firing on the same loops. Do not loosen this.

The corpus is not vendored here. Fetch these from `rust-clippy/tests/ui/` into
a `clippy/` directory beside the script, annotations intact:

    filter_map_next.rs
    manual_filter_map.rs
    manual_find.rs
    manual_find_fixable.rs
    manual_find_map.rs
    manual_flatten.rs
    manual_retain.rs
    manual_split_once.rs
    manual_strip.rs
    manual_unwrap_or.rs
    manual_while_let_some.rs
    needless_range_loop.rs
    search_is_some.rs

Paths to the infact binary, parser packs and behavior packs are constants at the
top of the file.

## pack-collisions.py

Groups a pack's behaviors by their normalized form and reports the ones shared
by differently-named callables.

The split that matters is whether the colliding callables share a *container*.
Different containers — `std::Entry::key` and `alloc::Entry::key` — are one
behavior on two types, and type information answers which. The **same**
container — `Result::into_ok` and `into_err` — is a distinction normalization
erased, and no type information will ever separate them. Those are bugs, and a
type-aware resolver would absorb them while looking authoritative.

`crates/infact-rust-behaviors/tests/collisions.rs` enforces the same rule over
itertools on every build. This script is for running it over a std pack, which
is too large to derive in a test.
