#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
//! Score the decision list against a hand-built classification.
//!
//! ```sh
//! cargo run -p infact-zig-lifetimes --example score -- \
//!     <bun-src-root> <LIFETIMES.tsv> <parser-pack-dir>
//! ```
//!
//! Reports per-class precision and recall, never a bare accuracy: a classifier
//! that nails `FFI` and misses every `OWNED` looks respectable in aggregate and
//! is useless for the port, because `OWNED` and `INTRUSIVE` are where a wrong
//! answer double-frees. The trivial baseline — always answer `BORROW_PARAM` —
//! is printed alongside, because a score quoted without it means nothing.
//!
//! A hit is an exact class match. Nothing else counts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use entl_tree_sitter::{LoadedParser, ParserPack, ParserRuntime};
use entl_zig_observe::fields;
use infact_zig_lifetimes::{OwnershipClass, RULES, classify};

/// How many held-out buckets the per-rule stability check uses.
const FOLDS: usize = 5;

/// Which bucket a file falls in. A stable hash of the path, so the split does
/// not move between runs and a fold is reproducible without storing it.
fn fold(file: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in file.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % FOLDS as u64) as usize
}

/// One row of the classification being scored against.
struct Row {
    file: String,
    container: String,
    field: String,
    class: OwnershipClass,
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
                class: OwnershipClass::parse(cells[4])?,
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

    // Observe every file the classification names, once.
    let mut files: Vec<&String> = rows.iter().map(|row| &row.file).collect();
    files.sort_unstable();
    files.dedup();

    // (file, container, field) -> observed field
    let mut observed = BTreeMap::new();
    let mut parsed_files = 0usize;
    for file in &files {
        let path = root.join(file);
        let Ok(source) = std::fs::read(&path) else {
            continue;
        };
        let Ok(tree) = parser.parse(&path, Arc::<[u8]>::from(source)) else {
            continue;
        };
        parsed_files += 1;
        for field in fields(&tree) {
            observed.insert(
                ((*file).clone(), field.container.clone(), field.name.clone()),
                field,
            );
        }
    }

    let mut matched = 0usize;
    let mut fired = 0usize;
    let mut correct = 0usize;
    // class -> (support, predicted, true positive)
    let mut tally: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();
    let mut per_rule: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    // Per rule, per fold: (fired, correct). Folds are five buckets of files, so
    // a rule carried by one subsystem shows up as a fold it fails on.
    let mut folds: BTreeMap<&'static str, [(usize, usize); FOLDS]> = BTreeMap::new();
    let mut leaks = 0usize;
    let mut unmatched: Vec<&Row> = Vec::new();

    for row in &rows {
        let key = (row.file.clone(), row.container.clone(), row.field.clone());
        let Some(field) = observed.get(&key) else {
            unmatched.push(row);
            continue;
        };
        matched += 1;
        tally.entry(row.class.label()).or_default().0 += 1;
        let Some(found) = classify(field) else {
            continue;
        };
        fired += 1;
        tally.entry(found.class.label()).or_default().1 += 1;
        let rule = per_rule.entry(found.rule.id()).or_default();
        rule.0 += 1;
        let bucket = &mut folds.entry(found.rule.id()).or_default()[fold(&row.file)];
        bucket.0 += 1;
        if found.class == row.class {
            correct += 1;
            rule.1 += 1;
            bucket.1 += 1;
            tally.entry(row.class.label()).or_default().2 += 1;
        } else if row.class.is_owning() {
            leaks += 1;
        }
    }

    println!("files named by the classification : {}", files.len());
    println!("files parsed                      : {parsed_files}");
    println!("rows in the classification        : {}", rows.len());
    println!(
        "rows matched to an observed field : {matched} ({:.1}%)",
        matched as f64 / rows.len() as f64 * 100.0
    );
    println!("rows the observer did not reach   : {}", unmatched.len());

    if std::env::var_os("SCORE_DIAGNOSE").is_some() {
        let mut by_depth: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        for row in &rows {
            let depth = row.container.matches('.').count();
            by_depth.entry(depth).or_default().0 += 1;
        }
        for row in &unmatched {
            by_depth
                .entry(row.container.matches('.').count())
                .or_default()
                .1 += 1;
        }
        println!("\n-- match rate by container-path depth --");
        for (depth, (total, missed)) in &by_depth {
            println!(
                "  {depth} dots: {total:5} rows, {missed:5} missed ({:.0}% matched)",
                (total - missed) as f64 / *total as f64 * 100.0
            );
        }
        println!("\n-- first unmatched rows, with what was observed in that file --");
        for row in unmatched.iter().take(6) {
            println!("  WANT {} | {} | {}", row.file, row.container, row.field);
            let mut shown = 0;
            for (file, container, name) in observed.keys() {
                if *file == row.file && *name == row.field {
                    println!("       saw container {container:?}");
                    shown += 1;
                    if shown == 3 {
                        break;
                    }
                }
            }
            if shown == 0 {
                println!("       (field name never observed in that file)");
            }
        }
    }

    println!("\n-- coverage and precision, over matched rows --");
    println!(
        "the list answered                 : {fired} of {matched} ({:.1}%)",
        fired as f64 / matched.max(1) as f64 * 100.0
    );
    println!(
        "of those, exactly right           : {correct} ({:.1}% precision)",
        correct as f64 / fired.max(1) as f64 * 100.0
    );
    println!(
        "wrong AND the field truly owns    : {leaks} ({:.1}% of answers) -- leak risk",
        leaks as f64 / fired.max(1) as f64 * 100.0
    );
    println!(
        "wrong but both classes non-owning : {}",
        fired - correct - leaks
    );
    println!("  a double-free needs an OWNED or SHARED answer, which no rule can produce.");

    println!("\n-- per rule --");
    println!(
        "{:<18}{:>8}{:>10}{:>12}   per fold",
        "rule", "fired", "precision", "worst fold"
    );
    for rule in RULES {
        let (n, ok) = per_rule.get(rule.id()).copied().unwrap_or((0, 0));
        if n == 0 {
            println!("{:<18}{n:>8}{:>10}", rule.id(), "-");
            continue;
        }
        let buckets = folds.get(rule.id()).copied().unwrap_or_default();
        let mut worst = f64::NAN;
        let mut shown = Vec::new();
        for (fired, correct) in buckets {
            if fired == 0 {
                shown.push("-".to_string());
                continue;
            }
            let precision = correct as f64 / fired as f64 * 100.0;
            shown.push(format!("{precision:.0}"));
            if worst.is_nan() || precision < worst {
                worst = precision;
            }
        }
        println!(
            "{:<18}{n:>8}{:>9.1}%{:>11.0}%   {}",
            rule.id(),
            ok as f64 / n as f64 * 100.0,
            worst,
            shown.join(" ")
        );
    }

    println!("\n-- per class, over matched rows --");
    println!(
        "{:<14}{:>7}{:>10}{:>9}{:>9}",
        "class", "n", "answered", "prec", "recall"
    );
    let mut baseline_hits = 0usize;
    for (class, (support, predicted, hit)) in &tally {
        if *support == 0 && *predicted == 0 {
            continue;
        }
        let precision = if *predicted == 0 {
            f64::NAN
        } else {
            *hit as f64 / *predicted as f64 * 100.0
        };
        let recall = if *support == 0 {
            f64::NAN
        } else {
            *hit as f64 / *support as f64 * 100.0
        };
        println!(
            "{class:<14}{support:>7}{predicted:>10}{precision:>8.1}%{recall:>8.1}%",
            precision = precision,
            recall = recall
        );
        if *class == "BORROW_PARAM" {
            baseline_hits = *support;
        }
    }

    println!(
        "\ntrivial baseline (always BORROW_PARAM) over the same matched rows: {:.1}%",
        baseline_hits as f64 / matched.max(1) as f64 * 100.0
    );
    println!(
        "the list abstains on {} matched rows; those need allocation-to-free evidence,",
        matched - fired
    );
    println!("not a guess.");
}
