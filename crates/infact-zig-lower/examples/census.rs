//! What Zig a corpus actually contains, and whether the grammar can read it.
//!
//! Two things nobody had measured. **Parse health**: `baozi/parse-patches.json`
//! works around seven grammar holes it found by hand, which says nothing about
//! how many remain. An `ERROR` node swallows the declarations inside it, so a
//! file that half-parses silently contributes half a plan. **Composition**:
//! which constructs a translator would have to handle, ranked by what they
//! cost.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use infact_zig_lower::patch_private_fields;
use tree_sitter::Node;

fn zig_files(root: &std::path::Path, found: &mut Vec<PathBuf>) {
    if root.is_file() {
        found.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("node_modules" | ".git" | "zig-cache" | "zig-out")
            ) {
                zig_files(&path, found);
            }
        } else if path.extension().is_some_and(|extension| extension == "zig") {
            found.push(path);
        }
    }
}

#[derive(Default)]
struct Census {
    kinds: BTreeMap<String, u64>,
    /// Where a parse error sits, by the kind of node that contains it.
    error_contexts: BTreeMap<String, u64>,
    errors: u64,
    missing: u64,
    nodes: u64,
}

fn walk(node: Node<'_>, source: &[u8], census: &mut Census, parent: &str) {
    if node.is_error() {
        census.errors += 1;
        // The text the grammar choked on, which is what says what to fix.
        let text = std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default())
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let excerpt = text.chars().take(60).collect::<String>();
        *census
            .error_contexts
            .entry(format!("in {parent}: {excerpt}"))
            .or_default() += 1;
        return;
    }
    if node.is_missing() {
        census.missing += 1;
    }
    if node.is_named() {
        census.nodes += 1;
        *census.kinds.entry(node.kind().to_owned()).or_default() += 1;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, census, node.kind());
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
        .expect("parser runtime")
        .load(pack)
        .expect("loading zig parser");

    let patch = std::env::args().any(|argument| argument == "--patch");
    let mut files = Vec::new();
    for root in std::env::args().skip(1).filter(|a| !a.starts_with("--")) {
        zig_files(std::path::Path::new(&root), &mut files);
    }
    files.sort();

    let mut census = Census::default();
    let mut clean_files = 0u64;
    let mut damaged = Vec::new();
    let mut lines = 0u64;

    for path in &files {
        let Ok(source) = std::fs::read(path) else {
            continue;
        };
        let source = if patch {
            patch_private_fields(&source)
        } else {
            source
        };
        lines += source.iter().filter(|byte| **byte == b'\n').count() as u64;
        let Ok(parsed) = parser.parse(
            path.to_string_lossy().as_ref(),
            Arc::<[u8]>::from(source.as_slice()),
        ) else {
            continue;
        };
        let before = census.errors;
        walk(parsed.tree.root_node(), &source, &mut census, "source_file");
        if census.errors == before {
            clean_files += 1;
        } else {
            damaged.push((census.errors - before, path.clone()));
        }
    }

    println!("# corpus");
    println!("files                 {}", files.len());
    println!("lines                 {lines}");
    println!("named nodes           {}", census.nodes);
    println!();
    println!("# parse health");
    println!(
        "files parsing cleanly {clean_files} of {} ({:.1}%)",
        files.len(),
        100.0 * clean_files as f64 / files.len() as f64
    );
    println!("ERROR nodes           {}", census.errors);
    println!("MISSING nodes         {}", census.missing);
    println!();
    println!("# worst files");
    damaged.sort_by_key(|(count, path)| (std::cmp::Reverse(*count), path.clone()));
    for (count, path) in damaged.iter().take(12) {
        println!("{count:>5}  {}", path.display());
    }
    println!();
    println!("# what the grammar choked on");
    let mut contexts = census.error_contexts.into_iter().collect::<Vec<_>>();
    contexts.sort_by(|left, right| (right.1, left.0.clone()).cmp(&(left.1, right.0.clone())));
    for (context, count) in contexts.iter().take(20) {
        println!("{count:>5}  {context}");
    }
    println!();
    println!("# composition, ranked");
    let mut kinds = census.kinds.into_iter().collect::<Vec<_>>();
    kinds.sort_by(|left, right| (right.1, left.0.clone()).cmp(&(left.1, right.0.clone())));
    let mut running = 0u64;
    for (kind, count) in &kinds {
        running += count;
        println!(
            "{kind:<32} {count:>8}  cum {:>5.1}%",
            100.0 * running as f64 / census.nodes as f64
        );
    }
}
