//! Freeze what the discard analyzer reports today, field by field.
//!
//! Run against the CURRENT implementation before the query port, then again
//! after. Any difference is a regression unless it is one the port intends.
//!
//! Usage: cargo run --bin golden -- <file.rs> ...   > golden.txt
//!
//! Defaults to the Rust pack in the adjacent entl checkout. Point `PACK_DIR`
//! at another pack to freeze another language.

use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};

/// The sibling entl checkout, relative to this harness.
const DEFAULT_PACK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../entl/parser-packs/rust"
);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pack = Arc::new(ParserPack::load(
        std::env::var("PACK_DIR").unwrap_or_else(|_| DEFAULT_PACK.to_owned()),
    )?);
    let runtime = ParserRuntime::new()?;
    let parser = runtime.load(pack)?;
    let analyze = |f: &_| infact_errors::analyze_file(&parser, f);

    let mut rows = Vec::new();
    for target in std::env::args().skip(1) {
        let source = std::fs::read(&target)?;
        // Name the file by its basename so the recorded callable paths do not
        // embed the absolute path of whoever ran this.
        let name = PathBuf::from(&target)
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_default();
        let file = parser.parse(name, source)?;
        for found in analyze(&file)? {
            rows.push(format!(
                "{target}\t{}\t{:?}\t{:?}\t{:?}\t{:?}\ttest={}\t{}..{}\t{}",
                found.callable,
                found.form,
                found.containment,
                found.certainty,
                found.reach,
                found.in_test,
                found.span.start_byte.unwrap_or_default(),
                found.span.end_byte.unwrap_or_default(),
                found.expression.replace('\n', "\\n"),
            ));
        }
    }
    rows.sort();
    for row in &rows {
        println!("{row}");
    }
    eprintln!("{} discard rows", rows.len());
    Ok(())
}
