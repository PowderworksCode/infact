#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::path::{Path, PathBuf};

use infact_core::TokenNormalization;
use infact_duplication::{NearConfig, analyze_repository_near};

#[test]
fn finds_consistently_renamed_clones_but_excludes_exact_copies() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let report = analyze_repository_near(
        crate_root.join("tests/fixtures/near"),
        crate_root.join("../../../entl/parser-packs/rust"),
        NearConfig {
            min_tokens: 20,
            min_lines: 5,
            normalize_identifiers: true,
            normalize_literals: true,
            max_changed_percent: 50,
        },
    )
    .unwrap();

    assert_eq!(report.files_parsed, 3);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(report.clones.iter().any(|fact| {
        fact.value.left.path == Path::new("src/prices.rs")
            && fact.value.right.path == Path::new("src/scores.rs")
            && fact.value.changed_tokens > 0
            && fact
                .value
                .normalizations
                .contains(&TokenNormalization::Identifiers)
            && fact
                .value
                .normalizations
                .contains(&TokenNormalization::Literals)
    }));
    assert!(!report.clones.iter().any(|fact| {
        fact.value.left.path == Path::new("src/prices.rs")
            && fact.value.right.path == Path::new("src/prices_copy.rs")
    }));
}
