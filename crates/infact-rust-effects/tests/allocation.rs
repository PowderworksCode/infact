//! An allocation origin reaches every caller above it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_core::Effect;
use infact_rust_effects::analyze_repository_effects;

/// The point of the seed is what it reaches, not where it is written.
#[test]
fn allocation_propagates_to_every_caller() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs/rust")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let report = analyze_repository_effects(
        crate_root.join("tests/fixtures/allocation"),
        &discovery.catalog,
        &[],
    )
    .unwrap();

    let allocators = report
        .effects
        .iter()
        .filter(|fact| fact.value.effect == Effect::Allocate)
        .map(|fact| {
            fact.value
                .callable
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<BTreeSet<_>>();

    for expected in [
        "allocates_directly",
        "allocates_through_a_macro",
        "allocates_through_a_method",
        "caller",
        "distant_caller",
    ] {
        assert!(
            allocators.contains(expected),
            "{expected} in {allocators:?}"
        );
    }
    for unexpected in ["allocates_nothing", "clones_a_handle"] {
        assert!(
            !allocators.contains(unexpected),
            "{unexpected} in {allocators:?}"
        );
    }

    // the trace has to say where, not only that
    let direct = report
        .effects
        .iter()
        .find(|fact| {
            fact.value.effect == Effect::Allocate
                && fact.value.callable.ends_with("allocates_directly")
        })
        .expect("a direct allocation");
    assert_eq!(direct.value.origin, "rust:allocation:Vec::with_capacity");
    assert_eq!(direct.value.path.len(), 1);
}

/// A callable that allocates three times has three traces.
///
/// `evidence_path` returned the first seed it reached, so the other sites
/// surfaced one re-run at a time. Bounded by the number of seeds on the
/// callable, deliberately not by the number of routes through the call graph:
/// paths between two nodes are exponential and a distant caller does not need
/// three spellings of the same news.
#[test]
fn every_allocation_site_in_a_callable_is_reported() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs/rust")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let report = analyze_repository_effects(
        crate_root.join("tests/fixtures/allocation"),
        &discovery.catalog,
        &[],
    )
    .unwrap();

    let sites = report
        .effects
        .iter()
        .filter(|fact| {
            fact.value.effect == Effect::Allocate
                && fact.value.callable.ends_with("allocates_three_times")
        })
        .map(|fact| fact.value.origin.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        sites.len(),
        3,
        "one trace per allocation site, got {sites:?}"
    );

    // a distant caller still gets one, not one per site below it
    let distant = report
        .effects
        .iter()
        .filter(|fact| {
            fact.value.effect == Effect::Allocate && fact.value.callable.ends_with("distant_caller")
        })
        .count();
    assert_eq!(distant, 1, "reach is reported once, not once per origin");
}
