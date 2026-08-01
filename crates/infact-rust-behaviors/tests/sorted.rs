use std::collections::BTreeSet;
use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_core::{DerivedLibraryBehavior, ExternalCatalog};
use infact_rust_behaviors::{analyze_repository, derive_behavior};

#[test]
fn derives_and_matches_the_six_iterator_sorting_methods() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parser_discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs")]);
    assert!(
        parser_discovery.errors.is_empty(),
        "{:?}",
        parser_discovery.errors
    );
    let catalog: ExternalCatalog = serde_json::from_slice(
        &std::fs::read(
            crate_root.join("../../infact-packs/rust-itertools/api/itertools-0.15.0.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let methods = [
        "sorted",
        "sorted_by",
        "sorted_by_key",
        "sorted_unstable",
        "sorted_unstable_by",
        "sorted_unstable_by_key",
    ];
    let behaviors = methods.map(|method| {
        serde_json::from_slice::<DerivedLibraryBehavior>(
            &std::fs::read(crate_root.join(format!(
                "../../infact-packs/rust-itertools/behaviors/itertools-{}-0.15.0.json",
                method.replace('_', "-")
            )))
            .unwrap(),
        )
        .unwrap()
    });

    for (method, expected) in methods.into_iter().zip(&behaviors) {
        let derived = derive_behavior(
            crate_root.join("tests/fixtures/itertools-source"),
            &parser_discovery.catalog,
            &catalog,
            &format!("itertools::Itertools::{method}"),
        )
        .unwrap();
        assert_eq!(derived.program, expected.program);
    }

    let report = analyze_repository(
        crate_root.join("tests/fixtures/sorted"),
        &parser_discovery.catalog,
        std::slice::from_ref(&catalog),
        &behaviors,
        &[],
    )
    .unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.matches.len(), 6);
    assert_eq!(
        report
            .matches
            .iter()
            .map(|fact| fact.value.target.path().to_owned())
            .collect::<BTreeSet<_>>(),
        methods
            .iter()
            .map(|method| format!("itertools::Itertools::{method}"))
            .collect::<BTreeSet<_>>()
    );
}
