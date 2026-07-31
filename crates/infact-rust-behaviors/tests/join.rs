use std::path::{Path, PathBuf};

use entl_tree_sitter::ParserCatalog;
use infact_core::{ExternalCatalog, LibraryBehaviorPattern};
use infact_rust_behaviors::analyze_repository;

#[test]
fn derives_join_behavior_match_only_when_the_catalog_signature_is_available() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parser_discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs")]);
    assert!(
        parser_discovery.errors.is_empty(),
        "{:?}",
        parser_discovery.errors
    );
    let catalog: ExternalCatalog = serde_json::from_slice(
        &std::fs::read(
            crate_root.join("../../fact-packs/rust-itertools/api/itertools-0.15.0.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let repository = crate_root.join("tests/fixtures/join");

    let report = analyze_repository(
        &repository,
        &parser_discovery.catalog,
        std::slice::from_ref(&catalog),
        &[],
        &[],
    )
    .unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.matches.len(), 1);
    assert_eq!(report.matches[0].value.span.path, Path::new("src/lib.rs"));
    assert_eq!(
        report.matches[0].value.pattern,
        LibraryBehaviorPattern::IteratorCollectVecJoin
    );
    assert_eq!(
        report.matches[0].value.target.path(),
        "itertools::Itertools::join"
    );

    let mut unavailable = catalog;
    unavailable.callables.clear();
    let report = analyze_repository(
        repository,
        &parser_discovery.catalog,
        &[unavailable],
        &[],
        &[],
    )
    .unwrap();
    assert!(report.matches.is_empty());
}
