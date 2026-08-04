//! What the normalizer cannot read, over a real corpus.
//!
//! `Form::Opaque` is how a frontend admits it has no canonical shape for a
//! construct. That is the right thing to emit and the wrong thing to leave
//! uncounted: an opaque node matches nothing, so a frontend that produced them
//! everywhere would report a clean repository rather than an unread one. This
//! counts them, by kind, so the gaps are ranked rather than assumed.
//!
//! ```sh
//! cargo run -p infact-python-normalize --example coverage -- PACKS ROOT...
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParserCatalog, ParserRuntime};
use infact_normalize::Form;
use infact_python_normalize::normalize_file;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::thread::Builder::new()
        .stack_size(1 << 29)
        .spawn(run)?
        .join()
        .map_err(|_| "worker panicked")??;
    Ok(())
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let packs = PathBuf::from(arguments.next().ok_or("usage: coverage PACKS ROOT...")?);
    let roots: Vec<PathBuf> = arguments.map(PathBuf::from).collect();
    if roots.is_empty() {
        return Err("usage: coverage PACKS ROOT...".to_owned());
    }

    let discovery = ParserCatalog::discover([packs]);
    if !discovery.errors.is_empty() {
        return Err(format!("{:?}", discovery.errors));
    }
    let runtime = ParserRuntime::new().map_err(|e| e.to_string())?;

    let mut files = Vec::new();
    for root in &roots {
        collect(root, &mut files);
    }
    files.sort();

    let mut totals = Totals::default();
    let mut opaque: BTreeMap<String, usize> = BTreeMap::new();
    for path in files {
        let Some(pack) = discovery.catalog.resolve("python", &path) else {
            continue;
        };
        let Ok(source) = std::fs::read(&path) else {
            continue;
        };
        let parser = runtime.load(pack.clone()).map_err(|e| e.to_string())?;
        let parsed = parser
            .parse(path.clone(), source)
            .map_err(|e| e.to_string())?;
        if parsed.tree.root_node().has_error() {
            totals.rejected += 1;
            continue;
        }
        totals.files += 1;
        for function in normalize_file(&parsed) {
            totals.functions += 1;
            let form = function.form.simplify().canonical();
            totals.nodes += form.size() as usize;
            let before = totals.opaque;
            count_opaque(&form, &mut opaque, &mut totals.opaque);
            if totals.opaque > before {
                totals.functions_with_opaque += 1;
            }
        }
    }

    println!("files          {}", totals.files);
    println!("rejected       {}", totals.rejected);
    println!("functions      {}", totals.functions);
    println!("form nodes     {}", totals.nodes);
    println!(
        "opaque nodes   {} ({:.3}% of form nodes)",
        totals.opaque,
        percentage(totals.opaque, totals.nodes)
    );
    println!(
        "functions with at least one opaque node   {} ({:.2}%)",
        totals.functions_with_opaque,
        percentage(totals.functions_with_opaque, totals.functions)
    );
    println!("\n-- opaque kinds, most common first --");
    let mut ranked: Vec<_> = opaque.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    for (kind, count) in ranked {
        println!("{count:>9}  {kind}");
    }
    Ok(())
}

#[derive(Default)]
struct Totals {
    files: usize,
    rejected: usize,
    functions: usize,
    functions_with_opaque: usize,
    nodes: usize,
    opaque: usize,
}

fn percentage(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64) * 100.0 / (whole as f64)
}

fn count_opaque(form: &Form, kinds: &mut BTreeMap<String, usize>, total: &mut usize) {
    if let Form::Opaque { kind, .. } = form {
        *kinds.entry(kind.clone()).or_default() += 1;
        *total += 1;
    }
    for child in form.children() {
        count_opaque(child, kinds, total);
    }
}

fn collect(root: &Path, into: &mut Vec<PathBuf>) {
    if root.is_file() {
        into.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
            into.push(path);
        }
    }
}
