//! Which Tree-sitter node kinds a corpus actually uses, ranked.
//!
//! What to build, in the order it pays.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use tree_sitter::Node;

fn walk(node: Node<'_>, counts: &mut BTreeMap<String, u64>) {
    if node.is_named() {
        *counts.entry(node.kind().to_owned()).or_default() += 1;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(child, counts);
    }
}

fn rust_files(root: &std::path::Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "zig") {
            found.push(path);
        }
    }
}

fn main() {
    let pack = Arc::new(
        ParserPack::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/zig"),
        )
        .expect("zig parser pack"),
    );
    let parser = ParserRuntime::new()
        .expect("runtime")
        .load(pack)
        .expect("rust parser");
    let mut files = Vec::new();
    for root in std::env::args().skip(1) {
        rust_files(std::path::Path::new(&root), &mut files);
    }
    let mut counts = BTreeMap::new();
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(parsed) = parser.parse(
            path.to_string_lossy().as_ref(),
            Arc::<[u8]>::from(bytes.as_slice()),
        ) else {
            continue;
        };
        walk(parsed.tree.root_node(), &mut counts);
    }
    let mut ordered = counts.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| (right.1, left.0.clone()).cmp(&(left.1, right.0.clone())));
    let total: u64 = ordered.iter().map(|(_, count)| *count).sum();
    println!(
        "{} files, {total} named nodes, {} kinds",
        files.len(),
        ordered.len()
    );
    let mut running = 0u64;
    for (kind, count) in &ordered {
        running += count;
        println!(
            "{kind:<34} {count:>6}  cum {:>5.1}%",
            100.0 * running as f64 / total as f64
        );
    }
}
