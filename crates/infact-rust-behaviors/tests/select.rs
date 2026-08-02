#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Decisions compared as decisions rather than as syntax.
//!
//! A `match` used to survive normalization as a tree of opaque syntax, which
//! made the order of the arms into behavior and let a hole stand for an arm
//! that leaves the function. Both are fixed by giving a decision a form of its
//! own: arms sorted by what they name, and arms compared as values.
//!
//! The third test is the one that keeps the mechanism useful. `map_or` is
//! `match self { Some(t) => f(t), None => default }` — two named alternatives
//! and nothing else. It describes every way of consuming an `Option`, so it
//! matches almost all code that touches one, and reporting it is noise. A
//! behavior has to name at least as much as it leaves open.
//!
//! `if let` is the same decision spelled differently, so it reduces to the same
//! form — including the case the library names and the code leaves to `_`.

use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_rust_behaviors::{analyze_repository, derive_library, is_reportable};

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
        crate_root().join("tests/fixtures/option-source"),
        &parsers(),
        "tinycore",
        "0.1.0",
    )
    .unwrap()
}

fn behavior(library: &infact_rust_behaviors::DerivedLibrary, name: &str) -> String {
    library
        .behaviors
        .iter()
        .find(|behavior| behavior.callable_path.ends_with(name))
        .unwrap_or_else(|| panic!("{name} is derivable"))
        .program
        .to_string()
}

/// Arms are held in a canonical order, so writing them the other way round is
/// not a different behavior.
#[test]
fn the_order_of_the_arms_is_not_behavior() {
    let library = derived();
    assert_eq!(
        behavior(&library, "::unwrap_or"),
        "(select f0 (None) => f1 (Some v0) => v0)"
    );

    let report = analyze_repository(
        crate_root().join("tests/fixtures/manual-unwrap-or"),
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
    assert!(paths.contains(&"tinycore::Option::unwrap_or"), "{paths:?}");
}

/// `if let ... else` decides what `match` decides.
///
/// The library names `None`; the code writes `_`. A catch-all covers whatever
/// is not named beside it, which for a two-alternative type is exactly the case
/// the library spelled out.
#[test]
fn an_if_let_is_a_decision_and_a_catch_all_is_the_case_not_named() {
    let library = derived();
    let report = analyze_repository(
        crate_root().join("tests/fixtures/if-let-unwrap-or"),
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
    assert!(paths.contains(&"tinycore::Option::unwrap_or"), "{paths:?}");
}

/// An arm that never returns is not an arm that produces a value.
#[test]
fn a_panicking_arm_is_not_a_default() {
    let library = derived();
    let report = analyze_repository(
        crate_root().join("tests/fixtures/panicking-match"),
        &parsers(),
        &[library.catalog],
        &library.behaviors,
        &[],
    )
    .unwrap();
    assert!(report.matches.is_empty(), "{:?}", report.matches);
}

/// A behavior that leaves more open than it names describes every program.
#[test]
fn a_form_with_more_holes_than_anchors_is_not_reported() {
    let library = derived();
    let unwrap_or = library
        .behaviors
        .iter()
        .find(|behavior| behavior.callable_path.ends_with("::unwrap_or"))
        .unwrap();
    let map_or = library
        .behaviors
        .iter()
        .find(|behavior| behavior.callable_path.ends_with("::map_or"))
        .unwrap();

    assert!(
        is_reportable(&unwrap_or.program),
        "unwrap_or is concrete about `Some`: {}",
        unwrap_or.program
    );
    assert!(
        !is_reportable(&map_or.program),
        "map_or leaves both arms open, so it matches every use of an Option: {}",
        map_or.program
    );
}
