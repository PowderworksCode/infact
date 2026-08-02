//! What a method costs depends on what it was called on.
//!
//! Syntax cannot see a receiver's type, so the allocation table declines
//! `clone` and `to_owned` outright. A resolved destination names the type, and
//! these are the cases that separates.

use std::collections::BTreeSet;
use std::path::PathBuf;

use entl_semantics::SemanticObservations;
use infact_core::Effect;
use infact_rust_effects::analyze_observed_effects;

fn allocators() -> BTreeSet<String> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/receivers/observations.json");
    let observations: SemanticObservations =
        serde_json::from_slice(&std::fs::read(fixture).expect("fixture observations"))
            .expect("observations parse");
    let report = analyze_observed_effects(&observations, &[]).expect("analysis runs");
    report
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
        .collect()
}

/// The whole point: one method, two costs, told apart.
#[test]
fn a_receiver_decides_whether_cloning_allocates() {
    let allocators = allocators();
    assert!(
        allocators.contains("clones_a_string"),
        "cloning a String copies its buffer: {allocators:?}"
    );
    assert!(
        !allocators.contains("clones_a_handle"),
        "cloning an Arc bumps a count: {allocators:?}"
    );
}

#[test]
fn owning_a_borrow_allocates_only_when_the_owned_form_is_on_the_heap() {
    let allocators = allocators();
    assert!(allocators.contains("owns_a_str"), "{allocators:?}");
    assert!(!allocators.contains("owns_an_integer"), "{allocators:?}");
}

/// The container being built is written into the destination, so collecting
/// into a `Vec` and collecting into `()` stop looking alike.
#[test]
fn collecting_allocates_only_when_it_builds_a_container() {
    let allocators = allocators();
    assert!(allocators.contains("collects_a_vec"), "{allocators:?}");
    assert!(!allocators.contains("collects_nothing"), "{allocators:?}");
}

/// `format!` reaches the allocator through an ordinary resolved call here, so
/// the observed path needs no knowledge of the macro at all.
#[test]
fn a_format_resolves_to_the_function_that_allocates() {
    assert!(allocators().contains("formats"), "{:?}", allocators());
}
