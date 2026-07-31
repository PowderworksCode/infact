use std::path::{Path, PathBuf};

use entl_tree_sitter::ParserCatalog;
use infact_duplication::{ExactConfig, analyze_repository, analyze_repository_with_catalog};

#[test]
fn finds_equal_tokens_despite_comments_and_formatting() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository = crate_root.join("tests/fixtures/exact");
    let parser_pack = crate_root.join("../../../entl/parser-packs/rust");
    let report = analyze_repository(
        repository,
        parser_pack,
        ExactConfig {
            min_tokens: 12,
            min_lines: 4,
        },
    )
    .unwrap();

    assert_eq!(report.files_parsed, 2);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(report.clones.iter().any(|fact| {
        fact.value.left.path == Path::new("src/first.rs")
            && fact.value.right.path == Path::new("src/second.rs")
            && fact.value.tokens >= 12
    }));
}

#[test]
fn selects_typescript_and_tsx_packs_from_one_catalog() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository = crate_root.join("tests/fixtures/web");
    let discovery = ParserCatalog::discover([crate_root.join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);

    let report = analyze_repository_with_catalog(
        repository,
        &discovery.catalog,
        ExactConfig {
            min_tokens: 12,
            min_lines: 4,
        },
    )
    .unwrap();

    assert_eq!(report.files_parsed, 2);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert!(report.clones.iter().any(|fact| {
        fact.value.left.path == Path::new("src/first.ts")
            && fact.value.right.path == Path::new("src/second.tsx")
            && fact
                .derivation
                .inputs
                .iter()
                .any(|input| input.parser_id == "tree-sitter-typescript")
            && fact
                .derivation
                .inputs
                .iter()
                .any(|input| input.parser_id == "tree-sitter-tsx")
    }));
}
