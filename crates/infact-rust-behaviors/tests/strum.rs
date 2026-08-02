#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use entl_tree_sitter::ParserCatalog;
use infact_core::{DerivedMacroBehavior, LibraryTarget, MacroBehavior, StringCase};
use infact_rust_behaviors::{MacroDerivationRequest, analyze_repository, derive_macro_behavior};

#[test]
fn derives_strum_behaviors_and_finds_equivalent_manual_enums() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parser_discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs")]);
    assert!(
        parser_discovery.errors.is_empty(),
        "{:?}",
        parser_discovery.errors
    );
    let expansion_root = crate_root.join("tests/fixtures/strum-expansion");
    let derived = derive_macro_behavior(
        &parser_discovery.catalog,
        MacroDerivationRequest {
            macro_package: "strum",
            macro_version: "0.28.0",
            derive_path: "strum::Display",
            probe_source: &std::fs::read(expansion_root.join("probe.rs")).unwrap(),
            expansion: &std::fs::read(expansion_root.join("display.rs")).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(
        derived.behavior,
        MacroBehavior::EnumDisplay {
            case: StringCase::Kebab
        }
    );
    let as_ref = derive_macro_behavior(
        &parser_discovery.catalog,
        MacroDerivationRequest {
            macro_package: "strum",
            macro_version: "0.28.0",
            derive_path: "strum::AsRefStr",
            probe_source: &std::fs::read(expansion_root.join("as_ref_snake_probe.rs")).unwrap(),
            expansion: &std::fs::read(expansion_root.join("as_ref_snake.rs")).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(
        as_ref.behavior,
        MacroBehavior::EnumAsRefStr {
            case: StringCase::Snake
        }
    );
    let variant_array = derive_macro_behavior(
        &parser_discovery.catalog,
        MacroDerivationRequest {
            macro_package: "strum",
            macro_version: "0.28.0",
            derive_path: "strum::VariantArray",
            probe_source: &std::fs::read(expansion_root.join("variant_array_probe.rs")).unwrap(),
            expansion: &std::fs::read(expansion_root.join("variant_array.rs")).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(variant_array.behavior, MacroBehavior::EnumVariantArray);

    let artifacts = [
        "strum-display-kebab-0.28.0.json",
        "strum-as-ref-str-kebab-0.28.0.json",
        "strum-as-ref-str-snake-0.28.0.json",
        "strum-variant-array-0.28.0.json",
    ]
    .map(|filename| {
        serde_json::from_slice::<DerivedMacroBehavior>(
            &std::fs::read(
                crate_root
                    .join("../../infact-packs/rust-strum/macro-behaviors")
                    .join(filename),
            )
            .unwrap(),
        )
        .unwrap()
    });
    let repository = crate_root.join("tests/fixtures/strum");
    let report =
        analyze_repository(&repository, &parser_discovery.catalog, &[], &[], &artifacts).unwrap();

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.matches.len(), 4);
    let behavior_match = &report
        .matches
        .iter()
        .find(|fact| fact.value.target.path() == "strum::Display")
        .unwrap()
        .value;
    assert_eq!(behavior_match.span.path, Path::new("src/lib.rs"));
    assert_eq!(behavior_match.span.start_line, 2);
    assert_eq!(behavior_match.span.end_line, 22);
    assert!(matches!(
        behavior_match.target,
        LibraryTarget::DeriveMacro {
            ref package,
            ref version,
            ref path,
            ..
        } if package == "strum" && version == "0.28.0" && path == "strum::Display"
    ));
    // the matched derive identifies itself; there is no separate pattern name
    assert_eq!(
        report
            .matches
            .iter()
            .map(|fact| fact.value.target.path())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["strum::AsRefStr", "strum::Display", "strum::VariantArray"])
    );
    assert_eq!(
        report
            .matches
            .iter()
            .filter(|fact| fact.value.target.path() == "strum::AsRefStr")
            .count(),
        2
    );

    let without_artifact =
        analyze_repository(repository, &parser_discovery.catalog, &[], &[], &[]).unwrap();
    assert!(without_artifact.matches.is_empty());
}

/// Doc comments on variants must not hide an enum from macro matching.
///
/// `line_comment` is a NAMED child of `enum_variant_list`. A recognizer that
/// walks every named child and demands an `enum_variant` finds one that is not,
/// declines the whole enum, and reports nothing — for the shape most real Rust
/// takes. The query matches `enum_variant` nodes directly, so comments are
/// simply not variants, and coverage counts only the variants.
#[test]
fn a_documented_enum_is_still_matched() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parser_discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs")]);
    assert!(
        parser_discovery.errors.is_empty(),
        "{:?}",
        parser_discovery.errors
    );
    let artifacts = [
        "strum-display-kebab-0.28.0.json",
        "strum-as-ref-str-kebab-0.28.0.json",
    ]
    .map(|filename| {
        serde_json::from_slice::<DerivedMacroBehavior>(
            &std::fs::read(
                crate_root
                    .join("../../infact-packs/rust-strum/macro-behaviors")
                    .join(filename),
            )
            .unwrap(),
        )
        .unwrap()
    });
    let report = analyze_repository(
        crate_root.join("tests/fixtures/strum-documented"),
        &parser_discovery.catalog,
        &[],
        &[],
        &artifacts,
    )
    .unwrap();

    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(
        report
            .matches
            .iter()
            .map(|fact| fact.value.target.path())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["strum::AsRefStr", "strum::Display"]),
        "a doc-commented enum reports nothing when comments are counted as variants"
    );
}
