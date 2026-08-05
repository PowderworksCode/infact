//! Classify every container field under a Zig tree and emit the result as TSV,
//! in the column layout cowbird's evidence manifest consumes
//! (stem, struct, field, class, confidence, evidence, platform).
//!
//! Both tiers run: the declaration rules first (safe by construction — cannot
//! emit an owning class), then the assignment-evidence rules for what the
//! first tier declined. The `evidence` column records which rule fired and its
//! basis, so a wrong row is checkable rather than merely wrong.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{LoadedParser, ParserPack, ParserRuntime};
use entl_zig_observe::{assignments, fields, method_calls};
use infact_zig_lifetimes::{FieldEvidence, OwnershipClass, classify, classify_with_evidence};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("zig src root"));
    let pack = PathBuf::from(args.next().expect("parser pack dir"));
    let parser: LoadedParser = ParserRuntime::new()
        .unwrap()
        .load(Arc::new(ParserPack::load(pack).unwrap()))
        .unwrap();

    let mut paths = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "zig") {
                paths.push(p);
            }
        }
    }
    paths.sort();

    println!("stem\tstruct\tfield\tclass\tconfidence\tevidence\tplatform");
    for path in paths {
        let Ok(source) = std::fs::read(&path) else { continue };
        let Ok(tree) = parser.parse(&path, Arc::<[u8]>::from(source)) else { continue };
        // The UNIT PATH -- the path below the corpus root without `.zig` --
        // because a bare stem collides: Bun has 1,292 files and 1,233 distinct
        // stems, so 59 of them would share a key with another file.
        let stem = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let file_assignments = assignments(&tree);
        let file_calls = method_calls(&tree);
        for field in fields(&tree) {
            let hit = classify(&field).or_else(|| {
                let assigns: Vec<_> = file_assignments
                    .iter()
                    .filter(|a| a.field == field.name)
                    .cloned()
                    .collect();
                let calls: Vec<_> = file_calls
                    .iter()
                    .filter(|c| c.receiver.ends_with(&field.name))
                    .cloned()
                    .collect();
                classify_with_evidence(
                    &field,
                    FieldEvidence { assignments: &assigns, calls: &calls },
                )
            });
            let Some(c) = hit else { continue };
            let class: OwnershipClass = c.class.into();
            // Confidence tiers follow cowbird's convention by measured precision.
            // measured_precision is a percentage (90.3), not a fraction.
            let conf = if c.measured_precision >= 85.0 {
                "high"
            } else if c.measured_precision >= 70.0 {
                "med"
            } else {
                "low"
            };
            println!(
                "{stem}\t{}\t{}\t{}\t{conf}\t{} p={:.2}\t",
                field.container,
                field.name,
                class.label(),
                c.basis.id(),
                c.measured_precision,
            );
        }
    }
    let _ = BTreeMap::<u8, u8>::new(); // keep clippy quiet about unused import if pruned
}
