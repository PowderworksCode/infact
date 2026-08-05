//! Lift a tree, print it back, and report what it cost.
//!
//! Two numbers. **Fidelity** is whether the printed program is the same
//! program, and is checked by building it — the harness in `tools/` does that.
//! **Coverage** is how much of the tree is structure rather than held text,
//! and is the number that improves: a rewrite can only reach what is
//! structured.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use infact_rust_lower::syntax::{Coverage, Expr, Pat, Stmt};
use infact_rust_lower::{Block, lift_file, reprint_file};

fn rust_files(root: &std::path::Path, found: &mut Vec<PathBuf>) {
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
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// The first line of a held-verbatim node, for ranking what to build next.
fn head(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    line.chars().take(60).collect()
}

fn survey_block(block: &Block, held: &mut BTreeMap<String, u64>) {
    for statement in &block.statements {
        match statement {
            Stmt::Let {
                pattern,
                value,
                diverging,
                ..
            } => {
                survey_pattern(pattern, held);
                if let Some(value) = value {
                    survey(value, held);
                }
                if let Some(block) = diverging {
                    survey_block(block, held);
                }
            }
            Stmt::Expr { value, .. } => survey(value, held),
            Stmt::Item(_) | Stmt::Comment { .. } => {}
        }
    }
}

fn survey_pattern(pattern: &Pat, held: &mut BTreeMap<String, u64>) {
    if let Pat::Verbatim(text) = pattern {
        *held.entry(format!("pattern: {}", head(text))).or_default() += 1;
    }
}

fn survey(value: &Expr, held: &mut BTreeMap<String, u64>) {
    if let Expr::Verbatim(text) = value {
        *held.entry(format!("expr: {}", head(text))).or_default() += 1;
    }
    // Only the verbatim leaves matter for ranking; the walk below reaches them
    // through the structured nodes that hold them.
    match value {
        Expr::Field { value, .. }
        | Expr::Unary { operand: value, .. }
        | Expr::Reference { value, .. }
        | Expr::Cast { value, .. }
        | Expr::Try(value)
        | Expr::Await(value)
        | Expr::Parenthesized(value)
        | Expr::Index { value, .. } => survey(value, held),
        Expr::Call {
            function: first,
            arguments,
        }
        | Expr::MethodCall {
            receiver: first,
            arguments,
            ..
        } => {
            survey(first, held);
            for argument in arguments {
                survey(argument, held);
            }
        }
        Expr::Binary { left, right, .. }
        | Expr::Assign {
            target: left,
            value: right,
            ..
        } => {
            survey(left, held);
            survey(right, held);
        }
        Expr::Closure {
            parameters, body, ..
        } => {
            for parameter in parameters {
                survey_pattern(parameter, held);
            }
            survey(body, held);
        }
        Expr::If {
            consequence,
            alternative,
            ..
        } => {
            survey_block(consequence, held);
            if let Some(alternative) = alternative {
                survey(alternative, held);
            }
        }
        Expr::Match { scrutinee, arms } => {
            survey(scrutinee, held);
            for arm in arms {
                survey_pattern(&arm.pattern, held);
                if let Some(guard) = &arm.guard {
                    survey(guard, held);
                }
                survey(&arm.body, held);
            }
        }
        Expr::While { body, .. } | Expr::Loop { body, .. } | Expr::Block { body, .. } => {
            survey_block(body, held);
        }
        Expr::For {
            pattern,
            sequence,
            body,
            ..
        } => {
            survey_pattern(pattern, held);
            survey(sequence, held);
            survey_block(body, held);
        }
        Expr::Return(value) | Expr::Break { value, .. } => {
            if let Some(value) = value {
                survey(value, held);
            }
        }
        Expr::Tuple(parts) => {
            for part in parts {
                survey(part, held);
            }
        }
        Expr::Array { elements, repeat } => {
            for element in elements {
                survey(element, held);
            }
            if let Some(repeat) = repeat {
                survey(repeat, held);
            }
        }
        Expr::Range { start, end, .. } => {
            if let Some(start) = start {
                survey(start, held);
            }
            if let Some(end) = end {
                survey(end, held);
            }
        }
        Expr::Struct { fields, .. } => {
            for field in fields {
                match field {
                    infact_rust_lower::syntax::FieldInit::Named { value, .. }
                    | infact_rust_lower::syntax::FieldInit::Base(value) => survey(value, held),
                    infact_rust_lower::syntax::FieldInit::Shorthand(_) => {}
                }
            }
        }
        _ => {}
    }
}

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let write_into = arguments
        .iter()
        .position(|argument| argument == "--write-into")
        .map(|position| PathBuf::from(&arguments[position + 1]));
    // Rewriting where the file already is, which is what a whole workspace
    // needs: a flat destination would collide every `lib.rs` with every other.
    let in_place = arguments.iter().any(|argument| argument == "--in-place");
    let roots = arguments
        .iter()
        .take_while(|argument| !argument.starts_with("--"))
        .map(PathBuf::from)
        .collect::<Vec<_>>();

    let pack = Arc::new(
        ParserPack::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust"),
        )
        .expect("rust parser pack"),
    );
    let parser = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser");

    let mut files = Vec::new();
    for root in &roots {
        rust_files(root, &mut files);
    }
    files.sort();

    let mut coverage = Coverage::default();
    let mut held = BTreeMap::new();
    let mut bodies = 0u64;
    let mut unchanged = 0u64;

    for path in &files {
        let Ok(source) = std::fs::read(path) else {
            continue;
        };
        let Ok(parsed) = parser.parse(
            path.to_string_lossy().as_ref(),
            Arc::<[u8]>::from(source.as_slice()),
        ) else {
            continue;
        };
        let lifted = lift_file(&parsed);
        bodies += lifted.len() as u64;
        for body in &lifted {
            coverage.add(body.block.coverage());
            survey_block(&body.block, &mut held);
        }
        let printed = reprint_file(&source, &lifted);
        if printed.as_bytes() == source.as_slice() {
            unchanged += 1;
        }
        if in_place {
            std::fs::write(path, &printed).expect("rewriting source");
        } else if let Some(destination) = &write_into {
            let target = destination.join(path.file_name().unwrap_or_default());
            std::fs::write(&target, &printed).expect("writing reprinted source");
        }
    }

    println!("files                 {}", files.len());
    println!("bodies lifted         {bodies}");
    println!("files printed byte-identical  {unchanged}");
    println!();
    println!(
        "expressions           {} ({} held verbatim)",
        coverage.expressions, coverage.verbatim_expressions
    );
    println!(
        "patterns              {} ({} held verbatim)",
        coverage.patterns, coverage.verbatim_patterns
    );
    println!(
        "STRUCTURAL COVERAGE   {:.2}%",
        100.0 * coverage.structured()
    );
    println!();
    println!("# what is still held as text, by what it costs");
    let mut ordered = held.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| (right.1, left.0.clone()).cmp(&(left.1, right.0.clone())));
    for (what, count) in ordered.iter().take(25) {
        println!("{count:>6}  {what}");
    }
}
