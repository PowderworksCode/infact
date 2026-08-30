//! How often one form names more than one callable.
//!
//! `coverage` counts what the normalizer could not read. This counts something
//! it cannot tell you: whether what it DID read distinguishes anything. A
//! frontend that reduced every function to the same form would report full
//! coverage and match nothing usefully, so the two belong side by side.
//!
//! Callables are grouped by canonical form, and within a group counted as
//! distinct when their source differs once whitespace is stripped. That
//! over-counts deliberately — two functions differing only in parameter names
//! SHOULD share a form — so a group is a question to read rather than a defect
//! on its own. What the number is good for is movement: it fell from 2.66% to
//! the figure in `notes/todo.txt` when callees stopped resolving to holes.
//!
//! ```sh
//! cargo run --release -p infact-python-normalize --example ambiguity -- PACKS ROOT...
//! ```

// `notes/todo.txt` is a checked-in census file, so the module doc naming it is
// a citation of where a number came from, not deferred work. File-scoped
// because the rule matches the filename itself.
// straitjacket-allow-file:stray-todo

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParserCatalog, ParserRuntime};
use infact_normalize::{Form, MINIMUM_REPORTABLE_SIZE};
use infact_python_normalize::normalize_file;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::thread::Builder::new()
        .stack_size(1 << 29)
        .spawn(run)?
        .join()
        .map_err(|_| "worker panicked")??;
    Ok(())
}

#[derive(Default)]
struct Group {
    sources: BTreeSet<String>,
    names: BTreeSet<String>,
    total: usize,
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let packs = PathBuf::from(arguments.next().ok_or("usage: ambiguity PACKS ROOT...")?);
    let roots: Vec<PathBuf> = arguments.map(PathBuf::from).collect();
    if roots.is_empty() {
        return Err("usage: ambiguity PACKS ROOT...".to_owned());
    }

    let discovery = ParserCatalog::discover([packs]);
    if !discovery.errors.is_empty() {
        return Err(format!("{:?}", discovery.errors));
    }
    let runtime = ParserRuntime::new().map_err(|error| error.to_string())?;

    let mut files = Vec::new();
    for root in &roots {
        collect(root, &mut files);
    }
    files.sort();

    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut functions = 0usize;
    let mut below_floor = 0usize;
    let mut callees = Callees::default();

    for path in files {
        let Some(pack) = discovery.catalog.resolve("python", &path) else {
            continue;
        };
        let Ok(source) = std::fs::read(&path) else {
            continue;
        };
        let parser = runtime
            .load(pack.clone())
            .map_err(|error| error.to_string())?;
        let parsed = parser
            .parse(path.clone(), source.clone())
            .map_err(|error| error.to_string())?;
        if parsed.tree.root_node().has_error() {
            continue;
        }
        for found in normalize_file(&parsed) {
            functions += 1;
            let form = found.form.simplify().canonical();
            count_callees(&form, &mut callees);
            if form.size() < MINIMUM_REPORTABLE_SIZE {
                below_floor += 1;
                continue;
            }
            let start = found.start_byte as usize;
            let end = (found.end_byte as usize).min(source.len());
            let text = String::from_utf8_lossy(&source[start..end])
                .split_whitespace()
                .collect::<String>();
            let entry = groups.entry(form.to_string()).or_default();
            entry.sources.insert(text);
            entry.names.insert(found.name.clone());
            entry.total += 1;
        }
    }

    let reportable: usize = groups.values().map(|group| group.total).sum();
    let ambiguous: Vec<_> = groups
        .iter()
        .filter(|(_, group)| group.sources.len() > 1)
        .collect();
    let in_ambiguous: usize = ambiguous.iter().map(|(_, group)| group.total).sum();

    println!("functions             {functions}");
    println!(
        "below the size floor  {below_floor} ({:.1}%)",
        percentage(below_floor, functions)
    );
    println!("reportable            {reportable}");
    println!("distinct forms        {}", groups.len());
    println!(
        "forms naming more than one distinct source   {} ({:.2}% of forms)",
        ambiguous.len(),
        percentage(ambiguous.len(), groups.len())
    );
    println!(
        "callables inside such a form                 {in_ambiguous} ({:.2}% of reportable)",
        percentage(in_ambiguous, reportable)
    );
    println!(
        "\ncalls whose callee is a hole                 {} of {} ({:.1}%)",
        callees.free,
        callees.total,
        percentage(callees.free, callees.total)
    );
    println!(
        "calls whose callee is a resolved name         {} ({:.1}%)",
        callees.path,
        percentage(callees.path, callees.total)
    );

    let mut ranked: Vec<_> = ambiguous;
    ranked.sort_by_key(|(_, group)| std::cmp::Reverse(group.sources.len()));
    println!("\n-- widest groups: one form, many textually different callables --");
    for (form, group) in ranked.iter().take(10) {
        let names: Vec<_> = group.names.iter().take(6).map(String::as_str).collect();
        println!(
            "\n{} distinct sources, {} callables\n  names: {}\n  form:  {}",
            group.sources.len(),
            group.total,
            names.join(", "),
            truncate(form, 200)
        );
    }
    Ok(())
}

#[derive(Default)]
struct Callees {
    total: usize,
    free: usize,
    path: usize,
}

/// What the thing being called resolves to.
///
/// A free callee is the normalizer saying "something is called here and I do
/// not know what". Two constructors differing only in which class they build
/// are one form while both are holes, which is the erasure this counts.
fn count_callees(form: &Form, tally: &mut Callees) {
    if let Form::Call { callee, .. } = form {
        tally.total += 1;
        match **callee {
            Form::Free(_) => tally.free += 1,
            Form::Path(_) => tally.path += 1,
            _ => {}
        }
    }
    for child in form.children() {
        count_callees(child, tally);
    }
}

fn truncate(text: &str, at: usize) -> String {
    match text.char_indices().nth(at) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
}

fn percentage(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    (part as f64) * 100.0 / (whole as f64)
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
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("py") {
            into.push(path);
        }
    }
}
