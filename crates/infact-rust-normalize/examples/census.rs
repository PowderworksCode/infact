//! What a lift keeps and what it destroys, counted over real code.
//!
//! Two measurements, neither of which needs an emitter to exist:
//!
//! 1. **Census.** How often each `Form` variant appears. A variant that
//!    discards its source cannot be lowered, so its share of the corpus is a
//!    ceiling on any emitter.
//! 2. **Collision.** How many distinct functions reduce to one form. Two
//!    sources sharing a form is a proof of unrecoverability that holds against
//!    every possible lowering: no function can return two answers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use entl_tree_sitter::{LoadedParser, ParserPack, ParserRuntime};
use infact_normalize::{Form, Pattern};
use infact_rust_normalize::normalize_file;

fn parser_packs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust")
}

fn rust_files(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("target" | ".git")
            ) {
                rust_files(&path, found);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The name of a form's variant, for counting.
fn variant(form: &Form) -> &'static str {
    match form {
        Form::Local(_) => "Local",
        Form::Free(_) => "Free",
        Form::Literal => "Literal",
        Form::Constant(_) => "Constant",
        Form::Number(_) => "Number",
        Form::Construct(_) => "Construct",
        Form::Variant { .. } => "Variant",
        Form::Path(_) => "Path",
        Form::Field { .. } => "Field",
        Form::Method { .. } => "Method",
        Form::Call { .. } => "Call",
        Form::Traverse { .. } => "Traverse",
        Form::Transform { .. } => "Transform",
        Form::Sift { .. } => "Sift",
        Form::Retain { .. } => "Retain",
        Form::Accumulate { .. } => "Accumulate",
        Form::Collect { .. } => "Collect",
        Form::Assign { .. } => "Assign",
        Form::Binary { .. } => "Binary",
        Form::Lambda { .. } => "Lambda",
        Form::Let { .. } => "Let",
        Form::Branch { .. } => "Branch",
        Form::Select { .. } => "Select",
        Form::Return(_) => "Return",
        Form::Sequence(_) => "Sequence",
        Form::Opaque { .. } => "Opaque",
    }
}

fn walk(form: &Form, counts: &mut BTreeMap<String, u64>, opaque: &mut BTreeMap<String, u64>) {
    *counts.entry(variant(form).to_owned()).or_default() += 1;
    if let Form::Opaque { kind, .. } = form {
        *opaque.entry(kind.clone()).or_default() += 1;
    }
    for child in form.children() {
        walk(child, counts, opaque);
    }
}

/// The source text of a function, with whitespace flattened.
///
/// Two functions written identically but for layout are the same function, and
/// counting them as a collision would overstate the loss.
fn flattened(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Lifted {
    label: String,
    source: String,
    form: String,
    size: u32,
}

fn main() {
    let mut roots = std::env::args()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    }

    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let runtime = ParserRuntime::new().expect("parser runtime");
    let parser: LoadedParser = runtime.load(pack).expect("loading rust parser");

    let mut files = Vec::new();
    for root in &roots {
        rust_files(root, &mut files);
    }
    files.sort();

    let mut counts = BTreeMap::new();
    let mut opaque = BTreeMap::new();
    let mut lifted = Vec::new();
    let mut parse_failures = 0u64;

    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let source = String::from_utf8_lossy(&bytes).into_owned();
        let Ok(parsed) = parser.parse(
            path.to_string_lossy().as_ref(),
            Arc::<[u8]>::from(bytes.as_slice()),
        ) else {
            parse_failures += 1;
            continue;
        };
        for function in normalize_file(&parsed) {
            walk(&function.form, &mut counts, &mut opaque);
            let text = source
                .get(function.start_byte as usize..function.end_byte as usize)
                .unwrap_or_default()
                .to_owned();
            lifted.push(Lifted {
                label: format!(
                    "{}:{}:{}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    function.start_line,
                    function.name
                ),
                source: flattened(&text),
                size: function.form.size(),
                form: function.form.to_string(),
            });
        }
    }

    println!("# corpus");
    println!("files            {}", files.len());
    println!("parse failures   {parse_failures}");
    println!("functions        {}", lifted.len());
    println!();

    let total: u64 = counts.values().sum();
    println!("# form nodes by variant ({total} nodes)");
    let mut ordered = counts.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| (right.1, left.0).cmp(&(left.1, right.0)));
    for (name, count) in ordered {
        println!(
            "{name:<12} {count:>7}  {:>5.1}%",
            100.0 * *count as f64 / total as f64
        );
    }
    println!();

    println!("# opaque kinds");
    let mut ordered = opaque.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| (right.1, left.0).cmp(&(left.1, right.0)));
    for (name, count) in ordered.iter().take(30) {
        println!("{name:<32} {count:>7}");
    }
    println!();

    // Collision: distinct sources sharing one form.
    let mut by_form: BTreeMap<&str, Vec<&Lifted>> = BTreeMap::new();
    for entry in &lifted {
        by_form.entry(&entry.form).or_default().push(entry);
    }
    let mut colliding_functions = 0u64;
    let mut classes = Vec::new();
    for (form, entries) in &by_form {
        let mut distinct = entries
            .iter()
            .map(|entry| entry.source.as_str())
            .collect::<Vec<_>>();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() > 1 {
            colliding_functions += entries.len() as u64;
            classes.push((distinct.len(), entries[0].size, *form, entries));
        }
    }
    classes
        .sort_by_key(|(count, size, _, _)| (std::cmp::Reverse(*count), std::cmp::Reverse(*size)));

    println!("# collisions: distinct source texts reducing to one form");
    println!(
        "functions in a colliding class   {colliding_functions} of {} ({:.1}%)",
        lifted.len(),
        100.0 * colliding_functions as f64 / lifted.len() as f64
    );
    println!("colliding classes                {}", classes.len());
    println!();
    println!("## largest colliding forms (form size, distinct sources)");
    for (distinct, size, form, entries) in
        classes.iter().filter(|(_, size, _, _)| *size >= 6).take(12)
    {
        println!("\n-- size {size}, {distinct} distinct sources: {form}");
        let mut seen = Vec::new();
        for entry in entries.iter() {
            if seen.contains(&entry.source.as_str()) {
                continue;
            }
            seen.push(&entry.source);
            let shown = if entry.source.len() > 150 {
                format!("{}...", &entry.source[..150])
            } else {
                entry.source.clone()
            };
            println!("   {}  {shown}", entry.label);
            if seen.len() >= 3 {
                break;
            }
        }
    }

    // How many functions carry at least one form node that cannot name its source.
    let mut with_construct = 0u64;
    let mut with_opaque = 0u64;
    for entry in &lifted {
        if entry.form.contains("(construct ") {
            with_construct += 1;
        }
    }
    for entry in &lifted {
        if entry.form.contains("(token_tree") || entry.form.contains("macro:") {
            with_opaque += 1;
        }
    }
    println!();
    println!("# functions touching a destructive variant");
    println!(
        "contains Construct   {with_construct} ({:.1}%)",
        100.0 * with_construct as f64 / lifted.len() as f64
    );
    println!(
        "contains a macro     {with_opaque} ({:.1}%)",
        100.0 * with_opaque as f64 / lifted.len() as f64
    );
    let _ = Pattern::Ignored;
}
