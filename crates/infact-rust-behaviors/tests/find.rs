#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A loop matched against a library that contains no loop.
//!
//! `Iterator::find` is written as a fold over `ControlFlow` threaded through a
//! local helper. A caller who reimplements it writes a `for` with an early
//! return. Nothing the two have in common is visible in their syntax, and
//! compiling them does not help either — rustc declines to inline the fold and
//! LLVM unrolls the two into different loops.
//!
//! What relates them is rewriting: unfolding the helper, reading a fold with an
//! unused accumulator as a traversal, and reading `ControlFlow` as the escape it
//! encodes. Those are laws of the language, not facts about `find`, so this test
//! is really about whether one set of laws can serve every library.

use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_rust_behaviors::{analyze_repository, derive_library};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

fn derived() -> infact_rust_behaviors::DerivedLibrary {
    derive_library(
        crate_root().join("tests/fixtures/std-source"),
        &parsers(),
        "tinystd",
        "0.1.0",
    )
    .unwrap()
}

/// The fold reduces to the loop it is equivalent to.
#[test]
fn a_fold_over_control_flow_normalizes_to_a_traversal_that_returns() {
    let library = derived();
    let find = library
        .behaviors
        .iter()
        .find(|behavior| behavior.callable_path.ends_with("::find"))
        .expect("find is derivable");

    assert_eq!(
        find.program.to_string(),
        "(do (traverse f0 v1 (branch (call f1 v1) (return (variant Some v1)))) (variant None))",
        "the helper is unfolded, the fold is a traversal, and the break is a return"
    );
}

/// The whole point: a hand-written loop is recognized as `find`.
#[test]
fn a_hand_written_loop_matches_a_library_that_contains_no_loop() {
    let library = derived();
    let report = analyze_repository(
        crate_root().join("tests/fixtures/manual-find"),
        &parsers(),
        &[library.catalog],
        &library.behaviors,
        &[],
    )
    .unwrap();

    let paths = report
        .matches
        .iter()
        .map(|fact| fact.value.target.path())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"tinystd::Iterator::find"), "{paths:?}");
}

/// A predicate that can leave the loop is not a predicate.
///
/// `find` applies whatever the caller passes and uses the answer. A test
/// containing `?` returns from the enclosing function instead, so no argument
/// to `find` could stand for it and the loop does something `find` cannot.
#[test]
fn a_loop_whose_test_escapes_is_not_a_search() {
    let library = derived();
    let report = analyze_repository(
        crate_root().join("tests/fixtures/escaping-find"),
        &parsers(),
        &[library.catalog],
        &library.behaviors,
        &[],
    )
    .unwrap();

    assert!(report.matches.is_empty(), "{:?}", report.matches);
}
