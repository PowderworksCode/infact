//! What a consumer sees: a catalog over ALL packs, run on a mixed repository.
//!
//! This is the one that shows a language arriving as data. Run it on a
//! repository holding two languages, and the discards from both appear without
//! anything here naming either.
//!
//! Usage: cargo run --bin repo -- <repository-root>
use entl_tree_sitter::ParserCatalog;
use std::path::PathBuf;

/// Every pack in the sibling entl checkout, relative to this harness.
const PACKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../entl/parser-packs");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths = vec![PathBuf::from(
        std::env::var("PACKS_DIR").unwrap_or_else(|_| PACKS.to_owned()),
    )];
    let discovery = ParserCatalog::discover(paths);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let root = std::env::args().nth(1).expect("a repository root");
    let report = infact_errors::analyze_repository_errors(&root, &discovery.catalog)?;
    for fact in &report.discards {
        println!(
            "{}  {:?}  {}  {:?}",
            fact.value.span.path.display(),
            fact.value.form,
            fact.value.callable,
            fact.value.reach
        );
    }
    println!("{} discards", report.discards.len());
    Ok(())
}
