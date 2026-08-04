# discard-golden

How the error-discard work is measured. Not part of the build.

`infact-errors` used to recognize Rust syntax in Rust code. It now reads
`discards.scm` and `callables.scm` from the parser pack, and knows no node
kinds of its own. These three binaries are how that move was shown to change
nothing, and how the next such move can be shown the same way.

The discipline they exist to support: **capture the output before touching
anything, then diff.** A port that "looks equivalent" is not equivalent. The
first capture of this analyzer differed from the second in 32 lost and 18
gained sites, from three separate causes, none of which were visible by
reading the diff.

## golden

Freezes what the analyzer reports, field by field — callable, form,
containment, certainty, reach, test-ness, byte span, expression text — one
sorted line each.

    cargo run --bin golden -- $(find ../../crates -name '*.rs') > before.txt
    # ...make the change...
    cargo run --bin golden -- $(find ../../crates -name '*.rs') > after.txt
    diff before.txt after.txt

Defaults to the Rust pack in the sibling entl checkout. `PACK_DIR=` points it
at another pack, which is all it takes to freeze another language.

Two cautions learned the hard way. Diff **structurally** — key on
file+form+span and count lost/gained/changed, because a raw line diff reports
every downstream shift as a difference and buries the real ones. And exclude
files you edited yourself during the port: your own edits move byte offsets,
so the spans shift and the tool reports a regression that is only your
changed corpus.

## errors

The experiment that motivated the port: rebuilds the analyzer's scaffolding
from queries alone — naming no Rust node kind — and diffs it against
`infact_errors::analyze_file` over real files.

    cargo run --bin errors -- $(find ../../crates -name '*.rs')
    DUMP=1 cargo run --bin errors -- one.rs    # print the parse tree

It reads the queries from the live pack rather than a vendored copy, so it
still earns its keep after the port: a change to `discards.scm` that breaks
the correspondence shows up as a diff instead of as silence. It compares only
the forms whose recognition is fully query-expressible — `LetUnderscore`,
`OkBinding`, `ErrArm`, at `Certain` — so a file holding none of those
correctly reports `0 agree, 0 differ`. That is not a broken run.

`DUMP=1` is the fastest way to answer "why doesn't my query match", which is
almost always that a node is named where you assumed it was anonymous, or
sits behind a field you did not write.

## repo

What a consumer actually sees: a catalog over **all** packs, run across a
whole repository.

    cargo run --bin repo -- ../../../straitjacket

This is the one that shows a language arriving as data. Run it on a
repository holding two languages and the discards from both appear, with
nothing here naming either — which is also the blast radius to check before
adding a pack, since every scanned repository gains that language at once.
