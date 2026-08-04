# Measurement harnesses

None of these is part of the build. They are how the behavior work is
measured, and the first two were reconstructed from memory more than once
before being put here.

## clippy-scoreboard.py

Scores our findings against clippy's own ui tests, which carry their ground
truth inline as `//~^ lint_name` annotations — a labelled corpus nobody wrote
for us.

A finding is credited only when it covers an annotated line **and** names an API
that lint is about. Requiring only overlap is not a metric: an earlier version
scored `manual_flatten` at 10/10 on the strength of `Option::is_none_or`
firing on the same loops. Do not loosen this.

The corpus is not vendored here. Fetch these from `rust-clippy/tests/ui/` into
`<measure>/clippy/`, annotations intact:

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

Fetch them with:

    M=<measure>/clippy && mkdir -p $M
    for f in <the names above, without .rs>; do
      curl -sS -o $M/$f.rs \
        https://raw.githubusercontent.com/rust-lang/rust-clippy/master/tests/ui/$f.rs
    done

Clippy's master still yields the 201 annotated positives the recorded numbers
were measured against, so a fresh fetch stays comparable. Re-count before
trusting a score against a re-fetched corpus; upstream may add cases.

### Layout

Paths derive from the script's own location, so nothing is machine-specific:

    INFACT_ROOT          defaults to the repo this file sits in
    INFACT_PARSER_PATH   defaults to <repo>/../entl/parser-packs
    INFACT_MEASURE       defaults to <repo>/../measure

`<measure>` holds what is too large or too fetched to vendor:

    measure/clippy/            the 13 ui tests above
    measure/packs/<name>/      one directory per behavior pack; every pack with a
                               behaviors/ subdirectory is scored
    measure/bench-scan/        scratch, rewritten per file

The harness needs `cargo build --release -p infact-cli` first.

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

## depcorpus.py

Scans every crate unpacked under `~/.cargo/registry/src` with one behavior pack.

The clippy corpus says whether a change finds MORE. This says whether it finds
more of the same thing, which is a different question and the one that has twice
caught a change being technically right and useless.

**The number that matters is the share held by the noisiest API**, not the total.
`Option::is_none_or` once fired 1,390 times across five hundred crates and
`map_or` nine hundred; both were correct, both subsumed every narrower behavior,
and both had to be removed. One API holding most of the findings is that failure
however good the total looks.

The second number is the repeat multiplier — distinct `(crate, file, api)`
against raw findings. Reporting every occurrence rather than the first was the
largest recall win the Rust side has had, and the risk was that real code would
multiply the same way and drown the output. It does not; 1.36x when measured.

    cargo build --release -p infact-cli
    python3 tools/depcorpus.py <measure>/packs/std          # every crate
    python3 tools/depcorpus.py <measure>/packs/std 40       # a smoke test

Nothing is vendored, so two machines will not agree on the total unless they
agree on their registries. **Compare shares, and re-run the baseline pack rather
than trusting a total recorded elsewhere.** A full run over 778 crates takes
about forty minutes.

Recorded 08-04, over 778 crates with the 488-behavior std pack:

    findings                  1,828
    crates with >= 1 finding    293
    distinct APIs named          67
    top API   Option::unwrap_or 311 (17.0%)
    is_none_or                   48

## ts-scoreboard

Scores TypeScript behavior matches the way `clippy-scoreboard.py` scores Rust
ones, against a labelled corpus nobody wrote for us: the lint plugins' own rule
tests. `invalid` cases are annotated positives and `valid` cases are annotated
NEGATIVES — which clippy's ui tests do not provide at all, and which are what
keeps precision honest.

A case is credited only when a match NAMES THE API THE RULE ASKS FOR. Requiring
merely that something matched is not a metric; the Rust scoreboard read 24/201
that way once and it was fake. Do not loosen this.

Recorded on 08-02: **25/155 positives, 4/252 false positives**, over
`prefer-find`, `prefer-includes` and `prefer-array-find`. Two of the remaining
four false positives need type information the checker supplies but this harness
joins only per-file; see the note in `notes/todo.txt`.

Re-measured on 08-03 through `infact-ts-behaviors` rather than through a copy of
the pipeline written inside the harness: **unchanged, 25/155 and 4/252**, from
**89 callables and 19 behaviors, 16 of them reportable**. The harness now calls
`derive_library` and `analyze_repository`, so what it scores is what a consumer
would get, including spans, implementation evidence and digests.

`pack-collisions.py` runs over a TypeScript pack unchanged, and should be run
over one for the same reason it is run over std:

    infact behavior library <measure>/spidermonkey --language typescript \
      --package ecmascript --version local --parser-path ../entl/parser-packs \
      --output <measure>/packs-typescript/ecmascript --allow-unread --explain
    python3 tools/pack-collisions.py <measure>/packs-typescript/ecmascript/behaviors

Recorded 08-03: **19 distinct forms from 19 behaviors, zero collisions.**

Keep TypeScript packs OUT of `measure/packs/`. `clippy-scoreboard.py` scores
every pack with a `behaviors/` subdirectory under it, and handing Rust code a
set of JavaScript-derived behaviors measures something nobody asked about.

    (cd $M/ts-lints && npm install typescript@5)   # once
    node tools/ts-scoreboard/export.mjs            # rule tests -> cases.json
    cargo run --manifest-path tools/ts-scoreboard/Cargo.toml --bin ts-scoreboard \
      -- --typescript $(realpath $M/ts-lints/node_modules/typescript/lib/typescript.js)

`--typescript` must be ABSOLUTE. It is handed to entl's observer script, which
resolves it relative to ITSELF, so a relative path silently loses the checker —
and losing the checker only shows up as two extra false positives, which reads
like a regression in the matcher rather than a missing input.

Version 5, deliberately: `npm install typescript` now installs the Go port,
whose package has no `lib/typescript.js` and no compiler API to call.

Neither the library source nor the rule tests are vendored. Both are fetched
into `<measure>/`:

    measure/spidermonkey/      Array.js String.js Object.js Map.js, from
                               mozilla/gecko-dev js/src/builtin/. These are the
                               only engine builtins written in plain JavaScript,
                               which is why they and not V8's Torque. MPL-2.0:
                               derive from them locally, exactly as the Rust std
                               pack is derived from the local rustup toolchain.
                               What ships is the derived form and its digests,
                               never the source.
    measure/ts-lints/          prefer-find.test.ts and prefer-includes.test.ts
                               from typescript-eslint, prefer-array-find.js from
                               eslint-plugin-unicorn, plus the generated
                               cases.json.

Fetch them with:

    M=${INFACT_MEASURE:-../measure}
    mkdir -p $M/spidermonkey $M/ts-lints
    B=https://raw.githubusercontent.com/mozilla/gecko-dev/master/js/src/builtin
    for f in Array String Object Map; do curl -sS -o $M/spidermonkey/$f.js $B/$f.js; done
    T=https://raw.githubusercontent.com/typescript-eslint/typescript-eslint/main/packages/eslint-plugin/tests/rules
    for f in prefer-find prefer-includes; do curl -sS -o $M/ts-lints/$f.test.ts $T/$f.test.ts; done
    curl -sS -o $M/ts-lints/prefer-array-find.js \
      https://raw.githubusercontent.com/sindresorhus/eslint-plugin-unicorn/main/test/prefer-array-find.js

`export.mjs` parses those rule tests with the TypeScript compiler rather than by
pattern, because they are TypeScript. It finds one by resolving `typescript`
from the working directory first and from `<measure>/ts-lints` second, so an
install beside the corpus is enough; `--typescript <path>` or `$TYPESCRIPT`
overrides both. The same flag on the scorer selects the checker used to observe
receiver types.

Same layout as above:

    INFACT_MEASURE       defaults to <repo>/../measure
    INFACT_PARSER_PATH   defaults to <repo>/../entl/parser-packs
