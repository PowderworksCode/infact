#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Distinct callables must derive distinct behaviors.
//!
//! The Rust side of this test found eight bugs, all one pattern: a distinction
//! erased during normalization, producing hundreds of confidently wrong findings
//! that were invisible in the output. It is the cheapest bug detector in the
//! project, and TypeScript needed its own before any TypeScript behaviors were
//! derived at scale — the failure mode is identical and the normalizer is not.
//!
//! Every callable in the fixture is a top-level function, so they all share one
//! container. That is the strict setting on purpose: no type information will
//! ever separate two functions in a flat namespace, so any two differently-named
//! ones sharing a form is an erasure and nothing downstream can recover it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_ts_behaviors::derive_library;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

fn leaf(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// Whether two names describe the same operation with a different knob.
///
/// A longer name that extends a shorter one is the general shape of that, and
/// the plainer name is already preferred when reporting.
fn one_extends_the_other(left: &str, right: &str) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[test]
fn distinct_callables_derive_distinct_behaviors() {
    let library = derive_library(
        crate_root().join("tests/fixtures/builtins-source"),
        &parsers(),
        "ecmascript",
        "test",
    )
    .unwrap();
    assert!(
        library.unparsed.is_empty(),
        "the fixture must be readable, or this test passes by deriving nothing: {:?}",
        library.unparsed
    );

    let mut by_form: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for behavior in &library.behaviors {
        by_form
            .entry(behavior.program.to_string())
            .or_default()
            .push(&behavior.callable_path);
    }

    let erasures = by_form
        .iter()
        .filter_map(|(form, paths)| {
            let colliding = paths
                .iter()
                .enumerate()
                .flat_map(|(index, left)| {
                    paths[index + 1..].iter().map(move |right| (*left, *right))
                })
                .filter(|(left, right)| {
                    leaf(left) != leaf(right) && !one_extends_the_other(leaf(left), leaf(right))
                })
                .collect::<Vec<_>>();
            (!colliding.is_empty()).then_some((form, colliding))
        })
        .collect::<Vec<_>>();

    assert!(
        erasures.is_empty(),
        "callables sharing one form, which no type information can separate:\n{}",
        erasures
            .iter()
            .map(|(form, pairs)| format!(
                "  {}\n    {form}",
                pairs
                    .iter()
                    .map(|(left, right)| format!("{left} / {right}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The fixture has to actually derive, or the collision test above proves
/// nothing.
///
/// A normalizer change that made everything underivable would leave zero forms
/// to collide and read as a pass. This is the assertion that makes the other one
/// mean something.
#[test]
fn the_fixture_derives_the_behaviors_it_was_written_for() {
    let library = derive_library(
        crate_root().join("tests/fixtures/builtins-source"),
        &parsers(),
        "ecmascript",
        "test",
    )
    .unwrap();
    let derived = library
        .behaviors
        .iter()
        .map(|behavior| leaf(&behavior.callable_path))
        .collect::<Vec<_>>();
    for wanted in [
        "ArrayFind",
        "ArrayFindLast",
        "ArraySome",
        "ArrayEvery",
        "ArrayFilter",
        "ArrayFindIndex",
    ] {
        assert!(
            derived.contains(&wanted),
            "{wanted} did not derive; got {derived:?}\nskipped: {:?}",
            library.skipped
        );
    }
}

/// A wrapper describes the work its helper does.
///
/// A public API is frequently a one-line forward to a helper that takes the
/// receiver explicitly. Recording the wrapper's own form would describe a call
/// rather than a search, and following it is what makes a public name stand for
/// the behavior a caller is looking for.
#[test]
fn a_delegating_wrapper_is_followed_to_the_work() {
    let library = derive_library(
        crate_root().join("tests/fixtures/builtins-source"),
        &parsers(),
        "ecmascript",
        "test",
    )
    .unwrap();
    let indexof = library
        .behaviors
        .iter()
        .find(|behavior| leaf(&behavior.callable_path) == "ArrayIndexOf")
        .expect("ArrayIndexOf derives");
    let steps = indexof
        .implementation
        .iter()
        .map(|step| step.callable_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(steps, vec!["ArrayIndexOf", "ArrayIndexOfInternal"]);
}
