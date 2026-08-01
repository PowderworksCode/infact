#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end derivation and matching, with no per-API machinery involved.

use std::collections::BTreeSet;
use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_core::{ExternalCatalog, LibraryTarget};
use infact_rust_behaviors::{analyze_repository, derive_behavior};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

fn catalog() -> ExternalCatalog {
    serde_json::from_slice(
        &std::fs::read(
            crate_root().join("../../infact-packs/rust-itertools/api/itertools-0.15.0.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn behavior(callable: &str) -> infact_core::DerivedLibraryBehavior {
    derive_behavior(
        crate_root().join("tests/fixtures/itertools-source"),
        &parsers(),
        &catalog(),
        callable,
    )
    .unwrap()
}

/// Derivation follows a public wrapper into the helper that does the work,
/// without being told that this particular API delegates.
#[test]
fn derivation_follows_delegation_to_the_implementing_helper() {
    let derived = behavior("itertools::Itertools::counts");

    assert_eq!(
        derived.implementation.len(),
        2,
        "the wrapper and the helper it delegates to are both evidence"
    );
    assert_eq!(derived.callable_path, "itertools::Itertools::counts");
    assert_eq!(
        derived.program.to_string(),
        "(do (let v0 (construct HashMap)) \
         (traverse f0 v1 (assign += (method or_default (method entry v0 v1)) (num 1))) v0)"
    );
}

/// A hand-written loop matches a combinator implementation of the same
/// behavior. This is the whole point of the mechanism.
#[test]
fn a_repository_loop_matches_the_derived_library_behavior() {
    let report = analyze_repository(
        crate_root().join("tests/fixtures/counts"),
        &parsers(),
        &[catalog()],
        &[behavior("itertools::Itertools::counts")],
        &[],
    )
    .unwrap();

    let paths = report
        .matches
        .iter()
        .map(|fact| fact.value.target.path())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"itertools::Itertools::counts"), "{paths:?}");
    let matched = &report.matches[0].value;
    assert!(matched.span.path.ends_with("lib.rs"), "{matched:?}");
    assert!(matches!(matched.target, LibraryTarget::Callable { .. }));
}

/// Code that does something else must not match, or every finding is noise.
#[test]
fn unrelated_repository_code_does_not_match() {
    // the join fixture collects and joins; it counts nothing
    let report = analyze_repository(
        crate_root().join("tests/fixtures/join"),
        &parsers(),
        &[catalog()],
        &[behavior("itertools::Itertools::counts")],
        &[],
    )
    .unwrap();
    assert!(report.matches.is_empty(), "{:?}", report.matches);
}

/// Two behaviors that differ only in what the loop body does stay distinct, and
/// each is reported against the function that actually reimplements it.
#[test]
fn grouping_and_counting_are_matched_separately() {
    let counts = behavior("itertools::Itertools::counts");
    let group_map = behavior("itertools::Itertools::into_group_map");
    assert_ne!(
        counts.program, group_map.program,
        "incrementing an entry is not pushing to one"
    );

    let report = analyze_repository(
        crate_root().join("tests/fixtures/counts"),
        &parsers(),
        &[catalog()],
        &[counts, group_map],
        &[],
    )
    .unwrap();

    // the fixture reimplements counting in lib.rs and grouping in more.rs
    let located = report
        .matches
        .iter()
        .map(|fact| {
            (
                fact.value.target.path(),
                fact.value
                    .span
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert!(
        located.contains(&("itertools::Itertools::counts", "lib.rs")),
        "{located:?}"
    );
    assert!(
        located.contains(&("itertools::Itertools::into_group_map", "more.rs")),
        "{located:?}"
    );
    assert!(
        !located.contains(&("itertools::Itertools::counts", "more.rs")),
        "grouping must not be reported as counting: {located:?}"
    );
}

/// A loop that also does something else still counts, but not interchangeably.
///
/// The behavior is genuinely present, so hiding it would lose a real finding.
/// Replacing the loop with the library call is not a mechanical substitution,
/// so it cannot be reported as though it were.
#[test]
fn an_extra_effect_in_the_loop_makes_the_match_fused() {
    let report = analyze_repository(
        crate_root().join("tests/fixtures/counts"),
        &parsers(),
        &[catalog()],
        &[behavior("itertools::Itertools::counts")],
        &[],
    )
    .unwrap();
    let logging = report
        .matches
        .iter()
        .find(|fact| {
            fact.value
                .span
                .path
                .file_name()
                .is_some_and(|name| name == "not_counts.rs")
        })
        .expect("the loop that counts and logs is still counting");
    assert!(logging.value.fused, "counting alongside logging is fused");

    // the plain reimplementation is not weakened by the existence of fused ones
    let plain = report
        .matches
        .iter()
        .find(|fact| {
            fact.value
                .span
                .path
                .file_name()
                .is_some_and(|name| name == "lib.rs")
        })
        .expect("the plain loop still matches");
    assert!(!plain.value.fused);
}

/// A library spells one behavior several ways. Reporting every spelling
/// against the same code is noise, so only the plainest is named.
#[test]
fn behaviors_that_differ_only_in_spelling_are_reported_once() {
    let variants = [
        "itertools::Itertools::counts",
        "itertools::Itertools::counts_with_hasher",
    ]
    .map(behavior);
    assert_eq!(
        variants[0].program, variants[1].program,
        "the hasher is supplied by the caller, so the behavior is the same"
    );

    let report = analyze_repository(
        crate_root().join("tests/fixtures/counts"),
        &parsers(),
        &[catalog()],
        &variants,
        &[],
    )
    .unwrap();
    let counting = report
        .matches
        .iter()
        .filter(|fact| {
            fact.value
                .span
                .path
                .file_name()
                .is_some_and(|name| name == "lib.rs")
        })
        .map(|fact| fact.value.target.path())
        .collect::<Vec<_>>();
    assert_eq!(counting, ["itertools::Itertools::counts"]);
}

/// A combinator does its work in the type it returns, not where it is called.
#[test]
fn a_returned_adaptor_is_followed_into_its_implementation() {
    let derived = behavior("itertools::Itertools::map_into");
    let steps = derived
        .implementation
        .iter()
        .map(|evidence| evidence.callable_path.as_str())
        .collect::<Vec<_>>();
    assert!(
        steps.contains(&"next"),
        "derivation should reach the adaptor's iterator method: {steps:?}"
    );
}

/// Real code rarely ends a behavior by naming what it built; it uses the value.
/// Requiring that closing step would miss most genuine reimplementations.
#[test]
fn a_behavior_matches_when_its_result_is_consumed_rather_than_returned() {
    let report = analyze_repository(
        crate_root().join("tests/fixtures/consumed"),
        &parsers(),
        &[catalog()],
        &[behavior("itertools::Itertools::counts")],
        &[],
    )
    .unwrap();
    let matched = report
        .matches
        .iter()
        .map(|fact| fact.value.target.path())
        .collect::<Vec<_>>();
    assert_eq!(matched, ["itertools::Itertools::counts"]);
}

/// A finding has to point at the code, not the function around it.
#[test]
fn a_match_is_reported_against_the_statements_that_carry_it() {
    let report = analyze_repository(
        crate_root().join("tests/fixtures/consumed"),
        &parsers(),
        &[catalog()],
        &[behavior("itertools::Itertools::counts")],
        &[],
    )
    .unwrap();
    let span = &report.matches[0].value.span;
    // the fixture's function spans several lines; the behavior is the two
    // statements that build the map
    let width = span.end_line - span.start_line + 1;
    assert!(
        width <= 5,
        "expected the statements, got {width} lines ({}-{})",
        span.start_line,
        span.end_line
    );
    assert!(span.start_line > 4, "the report should skip the signature");
}

/// Matching requires a catalog that vouches for the behavior's provenance.
#[test]
fn a_behavior_without_its_catalog_is_not_reported() {
    let report = analyze_repository(
        crate_root().join("tests/fixtures/counts"),
        &parsers(),
        &[],
        &[behavior("itertools::Itertools::counts")],
        &[],
    )
    .unwrap();
    assert!(report.matches.is_empty(), "{:?}", report.matches);
}

#[test]
fn no_behaviors_means_no_matches() {
    let report = analyze_repository(
        crate_root().join("tests/fixtures/counts"),
        &parsers(),
        &[catalog()],
        &[],
        &[],
    )
    .unwrap();
    assert!(report.matches.is_empty());
}
