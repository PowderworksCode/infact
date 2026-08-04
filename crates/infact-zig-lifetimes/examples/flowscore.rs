#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
//! What the value-flow graph reaches, measured against a hand-built answer key.
//!
//! ```sh
//! cargo run --release -p infact-zig-lifetimes --example flowscore -- \
//!     <bun-src-root> <LIFETIMES.tsv> <parser-pack-dir>
//! ```
//!
//! This scores *reachability*, not classification: for each field the key names,
//! which origins does the graph trace it to? A field reaching an allocation is
//! not yet `OWNED` and a field reaching a parameter is not yet `BORROW_PARAM`.
//! The point is to see whether the origins line up with the classes before any
//! rule is written on top of them, and to size the gaps honestly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use entl_tree_sitter::{LoadedParser, ParserPack, ParserRuntime};
use entl_zig_observe::{
    assignments, call_sites, deferred, fields, functions, locals, parent_recoveries, returns,
};
use infact_zig_lifetimes::flow::{FieldKey, FileObservations, build};
use infact_zig_lifetimes::origin::{ORIGINS, Origin};

fn serde_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => {}
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn collect_zig(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_zig(&path, root, out);
        } else if path.extension().is_some_and(|e| e == "zig")
            && let Ok(relative) = path.strip_prefix(root)
        {
            out.push(relative.to_string_lossy().into_owned());
        }
    }
}

struct Row {
    file: String,
    container: String,
    field: String,
    class: String,
}

fn load(path: &Path) -> Vec<Row> {
    let text = std::fs::read_to_string(path).expect("reading the classification");
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('\t').collect();
            if cells.len() < 5 {
                return None;
            }
            Some(Row {
                file: cells[0].to_string(),
                container: cells[1].to_string(),
                field: cells[2].to_string(),
                class: cells[4].to_string(),
            })
        })
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("bun src root"));
    let tsv = PathBuf::from(args.next().expect("LIFETIMES.tsv"));
    let pack = PathBuf::from(args.next().expect("parser pack directory"));

    let rows = load(&tsv);
    let parser: LoadedParser = ParserRuntime::new()
        .unwrap()
        .load(Arc::new(ParserPack::load(pack).unwrap()))
        .unwrap();

    // Every Zig file under the root, not only the ones the key names. A
    // container's fields are assigned from wherever it is constructed, which is
    // routinely a different file from the one that declares it, so narrowing
    // the corpus to the key's files hides most of the assignments.
    let mut named: Vec<String> = Vec::new();
    collect_zig(&root.join("src"), &root, &mut named);
    named.sort();

    let mut observations = Vec::new();
    for file in &named {
        let path = root.join(file);
        let Ok(source) = std::fs::read(&path) else {
            continue;
        };
        // The key names files by their path under the root; the graph keys on
        // that same relative path so a row can find its field.
        let Ok(parsed) = parser.parse(PathBuf::from(file), Arc::<[u8]>::from(source)) else {
            continue;
        };
        observations.push(FileObservations {
            path: PathBuf::from(file),
            fields: fields(&parsed),
            functions: functions(&parsed),
            assignments: assignments(&parsed),
            calls: call_sites(&parsed),
            returns: returns(&parsed),
            locals: locals(&parsed),
            deferred: deferred(&parsed),
            recoveries: parent_recoveries(&parsed),
        });
    }

    let flow = build(&observations);
    let accounting = flow.accounting();
    println!("files observed     : {}", observations.len());
    println!("graph nodes        : {}", flow.node_count());
    println!("flow edges         : {}", flow.graph().flows().len());
    println!("seeds              : {}", flow.graph().seeds().len());
    println!("containers freeing : {}", flow.freeing_containers());
    println!("\n-- assignments accounted for --");
    println!("total              : {}", accounting.total);
    println!("  seeded (origin)  : {}", accounting.seeded);
    println!("  linked (in graph): {}", accounting.linked);
    println!("  call, resolvable : {}", accounting.ambiguous);
    println!("  call, external   : {}", accounting.external);
    println!("  unplaced         : {}", accounting.unknown);
    println!(
        "balances           : {}",
        if accounting.balances() { "yes" } else { "NO" }
    );
    let unplaced = flow.unplaced();
    println!("\n-- why an assignment reached no declared field --");
    println!("no enclosing fn    : {}", unplaced.no_enclosing_function);
    println!("not a method       : {}", unplaced.not_a_method);
    println!("foreign receiver   : {}", unplaced.foreign_receiver);
    println!("no such field      : {}", unplaced.no_such_field);
    println!("  ..chained recv   : {}", unplaced.chained_receiver);
    println!("  ..untyped local  : {}", unplaced.untyped_local);
    println!(
        "accounted          : {} of {}",
        unplaced.total(),
        accounting.unknown
    );

    // Origins per ground-truth class.
    let carried = flow.graph().propagate();
    let mut by_node: BTreeMap<u64, Vec<Origin>> = BTreeMap::new();
    for labelled in &carried {
        if let Some(origin) = Origin::of_label(labelled.label) {
            by_node.entry(labelled.node).or_default().push(origin);
        }
    }

    let (mut matched, mut with_inflow) = (0usize, 0usize);
    let mut reached = 0usize;
    // class -> (rows, reached, origin -> count)
    let mut table: BTreeMap<String, (usize, usize, BTreeMap<&'static str, usize>)> =
        BTreeMap::new();
    let mut freed_by_class: BTreeMap<String, usize> = BTreeMap::new();

    for row in &rows {
        let key = FieldKey {
            path: PathBuf::from(&row.file),
            container: row.container.clone(),
            name: row.field.clone(),
        };
        let Some(node) = flow.node(&key) else {
            continue;
        };
        matched += 1;
        if flow.has_inflow(node) {
            with_inflow += 1;
        }
        let entry = table.entry(row.class.clone()).or_default();
        entry.0 += 1;
        if flow.freed_by_container(&key) {
            *freed_by_class.entry(row.class.clone()).or_default() += 1;
        }
        let Some(origins) = by_node.get(&node) else {
            continue;
        };
        reached += 1;
        entry.1 += 1;
        for origin in origins {
            *entry.2.entry(origin.id()).or_default() += 1;
        }
    }

    println!("\n-- reachability, over rows the observer matched --");
    println!("rows in the key    : {}", rows.len());
    println!(
        "matched to a field : {matched} ({:.1}%)",
        matched as f64 / rows.len() as f64 * 100.0
    );
    println!(
        "has any inflow     : {with_inflow} ({:.1}% of matched)",
        with_inflow as f64 / matched.max(1) as f64 * 100.0
    );
    println!(
        "reached any origin : {reached} ({:.1}% of matched, {:.1}% of those with inflow)",
        reached as f64 / matched.max(1) as f64 * 100.0,
        reached as f64 / with_inflow.max(1) as f64 * 100.0
    );

    println!("\n-- origins reached, by the class the key gives --");
    print!("{:<14}{:>6}{:>9}{:>7}", "class", "rows", "reached", "freed");
    for origin in ORIGINS {
        print!("{:>11}", origin.id());
    }
    println!();
    for (class, (rows_in_class, reached_in_class, origins)) in &table {
        print!(
            "{class:<14}{rows_in_class:>6}{reached_in_class:>9}{:>7}",
            freed_by_class.get(class).copied().unwrap_or(0)
        );
        for origin in ORIGINS {
            print!("{:>11}", origins.get(origin.id()).copied().unwrap_or(0));
        }
        println!();
    }
    if let Some(out) = std::env::var_os("FLOW_DUMP_UNPLACED") {
        // Position of the receiver, zero-based line and character, which is
        // what an LSP position is. The receiver starts the assignment, so the
        // assignment span's start is the receiver's start.
        let mut entries = Vec::new();
        for site in flow.unplaced_sites() {
            if site.receiver.is_empty() {
                continue;
            }
            entries.push(format!(
                "{{\"file\":{},\"line\":{},\"character\":{},\"receiver\":{},\"field\":{},\"container\":{}}}",
                serde_escape(&site.path.display().to_string()),
                site.span.start_line - 1,
                0,
                serde_escape(&site.receiver),
                serde_escape(&site.field),
                serde_escape(&site.container),
            ));
        }
        std::fs::write(&out, format!("[{}]", entries.join(","))).expect("writing unplaced sites");
        println!(
            "\nwrote {} unplaced sites to {}",
            entries.len(),
            out.display()
        );
    }

    if std::env::var_os("FLOW_DIAGNOSE").is_some() {
        // For key fields with no inflow, show every assignment in the corpus
        // that writes a field of that name, and what receiver it went through.
        let mut by_field: BTreeMap<&str, Vec<(&str, &str, &str)>> = BTreeMap::new();
        for file in &observations {
            for assignment in &file.assignments {
                by_field
                    .entry(assignment.field.as_str())
                    .or_default()
                    .push((
                        assignment.receiver.as_str(),
                        assignment.container.as_str(),
                        assignment.value.as_str(),
                    ));
            }
        }
        println!("\n-- key fields with no inflow: what writes them, if anything --");
        let mut shown = 0;
        let mut never_written = 0;
        for row in &rows {
            let key = FieldKey {
                path: PathBuf::from(&row.file),
                container: row.container.clone(),
                name: row.field.clone(),
            };
            let Some(node) = flow.node(&key) else {
                continue;
            };
            if flow.has_inflow(node) {
                continue;
            }
            match by_field.get(row.field.as_str()) {
                None => never_written += 1,
                Some(writes) if shown < 12 => {
                    shown += 1;
                    println!(
                        "  {} | {}.{} [{}]",
                        row.file, row.container, row.field, row.class
                    );
                    for (receiver, container, value) in writes.iter().take(3) {
                        println!("      recv={receiver:?} container={container:?} value={value:?}");
                    }
                }
                _ => {}
            }
        }
        println!("  key fields whose name is never assigned anywhere: {never_written}");
    }

    println!("\nA field reaching an allocation is not yet OWNED and one reaching a");
    println!("parameter is not yet BORROW_PARAM. These are the inputs to that, not it.");
}
