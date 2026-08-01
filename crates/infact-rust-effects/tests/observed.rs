//! What resolved observations buy over syntax, measured on one fixture.
//!
//! The fixture writes the same effectful call four ways. Syntax can only
//! recognize the spelling that happens to match a catalog path; resolution
//! recognizes all of them. Both analyzers run here so the difference is a
//! test result rather than a claim.

use std::path::PathBuf;

use entl_semantics::SemanticObservations;
use entl_tree_sitter::ParserCatalog;
use infact_core::{CallEffectCatalog, Effect};
use infact_rust_effects::{analyze_observed_effects, analyze_repository_effects};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture() -> PathBuf {
    crate_root().join("tests/fixtures/imports")
}

fn catalogs() -> Vec<CallEffectCatalog> {
    let path = crate_root().join("../../infact-packs/rust-core/effects/rust-core-1.93.1.json");
    vec![serde_json::from_slice(&std::fs::read(path).expect("effect catalog")).expect("catalog")]
}

fn observations() -> SemanticObservations {
    let path = fixture().join("observations.json");
    serde_json::from_slice(&std::fs::read(path).expect("observations fixture")).expect("parse")
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

/// Every callable the analyzer says performs a file read, directly or not.
fn readers(effects: &[infact_core::Fact<infact_core::EffectTrace>]) -> Vec<String> {
    let mut names = effects
        .iter()
        .filter(|fact| fact.value.effect == Effect::FileRead)
        .map(|fact| {
            fact.value
                .callable
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

/// The gap this whole exercise exists to close.
#[test]
fn resolution_finds_the_reads_that_syntax_cannot_see() {
    let syntax = analyze_repository_effects(fixture(), &parsers(), &catalogs()).unwrap();
    let observed = analyze_observed_effects(&observations(), &catalogs()).unwrap();

    // syntax only recognizes the fully qualified spelling
    assert_eq!(
        readers(&syntax.effects),
        ["qualified"],
        "the import spellings are invisible to syntax"
    );

    // resolution recognizes all of them, and the caller that reaches one
    assert_eq!(
        readers(&observed.effects),
        ["caller", "qualified", "via_item", "via_module"]
    );
}

/// A call written through an import is the same call, and carries the same origin.
#[test]
fn an_imported_call_records_the_canonical_origin() {
    let observed = analyze_observed_effects(&observations(), &catalogs()).unwrap();
    let via_module = observed
        .effects
        .iter()
        .find(|fact| fact.value.callable.ends_with("via_module"))
        .expect("via_module performs a read");
    assert_eq!(via_module.value.origin, "std::fs::read");
    assert_eq!(via_module.value.effect, Effect::FileRead);
}

/// Effects still propagate to callers, which is what makes the graph worth having.
#[test]
fn an_effect_reaches_the_caller_that_triggers_it() {
    let observed = analyze_observed_effects(&observations(), &catalogs()).unwrap();
    let caller = observed
        .effects
        .iter()
        .find(|fact| fact.value.callable.ends_with("::caller"))
        .expect("caller reaches a read through via_module");
    assert_eq!(caller.value.origin, "std::fs::read");
    // the evidence names each step from the caller to the effect origin
    let steps = caller
        .value
        .path
        .iter()
        .map(|edge| edge.callee.as_str())
        .collect::<Vec<_>>();
    assert_eq!(steps, ["imports::via_module", "std::fs::read"]);
}

/// Code that does nothing effectful must stay clean, or the analysis is useless.
#[test]
fn a_pure_function_reports_no_effect() {
    let observed = analyze_observed_effects(&observations(), &catalogs()).unwrap();
    assert!(
        !observed
            .effects
            .iter()
            .any(|fact| fact.value.callable.ends_with("::pure")),
        "a function that only returns a literal performs no effect"
    );
}

/// Provenance has to say which compiler produced the graph.
#[test]
fn traces_record_the_provider_that_resolved_them() {
    let observed = analyze_observed_effects(&observations(), &catalogs()).unwrap();
    let fact = observed.effects.first().expect("some effect was found");
    assert_eq!(fact.derivation.analyzer, "infact-rust-effects.observed");
    let evidence = fact.derivation.inputs.first().expect("an input");
    assert_eq!(evidence.parser_id, "rust.mir");
    assert!(evidence.grammar_sha256.contains("rustc"));
}

/// Every call in the fixture resolves, so nothing is left unaccounted.
#[test]
fn the_observed_graph_accounts_for_every_call() {
    let observed = analyze_observed_effects(&observations(), &catalogs()).unwrap();
    assert_eq!(observed.calls.total, 4);
    assert_eq!(observed.calls.known_effect_origins, 3);
    assert_eq!(observed.calls.linked_internal, 1);
    assert!(
        observed.diagnostics.is_empty(),
        "{:?}",
        observed.diagnostics
    );
}
