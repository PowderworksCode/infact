use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use entl_tree_sitter::ParserCatalog;
use infact_core::{
    DerivedLibraryBehavior, ExternalCatalog, LibraryBehaviorPattern, NormalizedOperation,
    NormalizedValue,
};
use infact_rust_behaviors::{analyze_repository, derive_behavior};

#[test]
fn derives_counts_from_a_bound_accumulator_loop_and_catalog_signature() {
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
    let behaviors = [
        "itertools-counts-0.15.0.json",
        "itertools-counts-by-0.15.0.json",
        "itertools-into-group-map-0.15.0.json",
        "itertools-into-group-map-by-0.15.0.json",
    ]
    .map(|filename| {
        serde_json::from_slice::<DerivedLibraryBehavior>(
            &std::fs::read(
                crate_root
                    .join("../../fact-packs/rust-itertools/behaviors")
                    .join(filename),
            )
            .unwrap(),
        )
        .unwrap()
    });
    let repository = crate_root.join("tests/fixtures/counts");

    let derived = derive_behavior(
        crate_root.join("tests/fixtures/itertools-source"),
        &parser_discovery.catalog,
        &catalog,
        "itertools::Itertools::counts",
    )
    .unwrap();
    assert_eq!(derived.implementation.len(), 2);
    assert_eq!(
        derived.program.operations,
        vec![
            NormalizedOperation::CreateMap {
                output: NormalizedValue("map".to_owned()),
            },
            NormalizedOperation::Iterate {
                input: NormalizedValue("input".to_owned()),
                item: NormalizedValue("item".to_owned()),
                body: vec![NormalizedOperation::IncrementMapEntry {
                    map: NormalizedValue("map".to_owned()),
                    key: NormalizedValue("item".to_owned()),
                    amount: 1,
                }],
            },
            NormalizedOperation::Return {
                value: NormalizedValue("map".to_owned()),
            },
        ]
    );
    for (callable, expected) in [
        ("itertools::Itertools::counts", &behaviors[0]),
        ("itertools::Itertools::counts_by", &behaviors[1]),
        ("itertools::Itertools::into_group_map", &behaviors[2]),
        ("itertools::Itertools::into_group_map_by", &behaviors[3]),
    ] {
        let derived = derive_behavior(
            crate_root.join("tests/fixtures/itertools-source"),
            &parser_discovery.catalog,
            &catalog,
            callable,
        )
        .unwrap();
        assert_eq!(derived.program, expected.program);
    }

    let report = analyze_repository(
        &repository,
        &parser_discovery.catalog,
        std::slice::from_ref(&catalog),
        &behaviors,
        &[],
    )
    .unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.matches.len(), 4);
    let behavior_match = report
        .matches
        .iter()
        .find(|fact| fact.value.target.path() == "itertools::Itertools::counts")
        .unwrap();
    let behavior_match = &behavior_match.value;
    assert_eq!(behavior_match.span.path, Path::new("src/lib.rs"));
    assert_eq!(behavior_match.span.start_line, 4);
    assert_eq!(behavior_match.span.end_line, 8);
    assert_eq!(
        behavior_match.pattern,
        LibraryBehaviorPattern::IteratorManualCounts
    );
    assert_eq!(behavior_match.target.path(), "itertools::Itertools::counts");
    assert_eq!(
        report
            .matches
            .iter()
            .map(|fact| fact.value.target.path())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "itertools::Itertools::counts",
            "itertools::Itertools::counts_by",
            "itertools::Itertools::into_group_map",
            "itertools::Itertools::into_group_map_by",
        ])
    );

    let mut incompatible = catalog.clone();
    incompatible
        .callables
        .retain(|callable| callable.path != "itertools::Itertools::counts");
    let report = analyze_repository(
        &repository,
        &parser_discovery.catalog,
        &[incompatible],
        &behaviors,
        &[],
    )
    .unwrap();
    assert!(
        report
            .matches
            .iter()
            .all(|fact| fact.value.target.path() != "itertools::Itertools::counts")
    );

    let report =
        analyze_repository(repository, &parser_discovery.catalog, &[catalog], &[], &[]).unwrap();
    assert!(report.matches.is_empty());
}
