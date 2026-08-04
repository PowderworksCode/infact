//! One operation implemented in two packages.
//!
//! `coverage` counts what the normalizer could not read and `ambiguity` counts
//! whether what it read distinguishes anything. This asks the question those
//! two exist to make askable: when two functions in different distributions
//! reduce to the same form, they are two implementations of one operation, and
//! that is the signal `library-opportunity` is built on.
//!
//! It only became worth running once a called name resolved. While 94.9% of
//! callees were holes, any two functions that forwarded a call matched, and a
//! shared form was an artifact rather than evidence.
//!
//! ```sh
//! cargo run --release -p infact-python-normalize --example duplicates -- PACKS ROOT
//! ```
//!
//! ## Reading the output
//!
//! The funnel matters more than the headline. Measured over the installed
//! site-packages, the three lines were 1,791 / 207 / 5: most cross-package
//! matches are VENDORING, where a project ships a copy of a library under
//! `_vendor/`, and reporting those says only that pip vendors things. Most of
//! what survives that is one library's own helper reused under one name, which
//! is why the last line asks whether the names differ.
//!
//! Of what survives, distinguish two kinds, because they are worth different
//! amounts:
//!
//!   - A COPY. `mitmproxy`'s `rle_append_beginning_modify` and `urwid`'s
//!     `rle_prepend_modify` are byte-identical but for the name. Real, and a
//!     token-based clone detector run across both trees would also find it,
//!     because `infact-duplication` normalizes identifiers.
//!   - AN INDEPENDENT IMPLEMENTATION. `ruamel.yaml`'s `construct_yaml_pairs`
//!     and `PyYAML`'s `construct_yaml_omap` share a form while differing in
//!     names, string literals, formatting and error messages. Nothing
//!     token-based pairs those. They meet because the normalizer drops what
//!     only raises, so the difference between their two error paths is
//!     correctly not behavior.
//!
//! Only the second kind is beyond what duplication analysis already does, so
//! it is the one to read the list for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParserCatalog, ParserRuntime};
use infact_normalize::MINIMUM_REPORTABLE_SIZE;
use infact_python_normalize::normalize_file;

/// Twice the reportable floor.
///
/// A form at the floor collides across unrelated code by design, and a claim
/// that two LIBRARIES implement one operation should rest on more evidence
/// than a claim about two functions in one file.
const MINIMUM_SHARED_SIZE: u32 = MINIMUM_REPORTABLE_SIZE * 2;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::thread::Builder::new()
        .stack_size(1 << 29)
        .spawn(run)?
        .join()
        .map_err(|_| "worker panicked")??;
    Ok(())
}

struct Site {
    package: String,
    display: String,
    name: String,
    source: String,
    size: u32,
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let packs = PathBuf::from(arguments.next().ok_or("usage: duplicates PACKS ROOT")?);
    let root = PathBuf::from(arguments.next().ok_or("usage: duplicates PACKS ROOT")?);

    let discovery = ParserCatalog::discover([packs]);
    if !discovery.errors.is_empty() {
        return Err(format!("{:?}", discovery.errors));
    }
    let runtime = ParserRuntime::new().map_err(|error| error.to_string())?;

    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();

    let mut groups: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    let mut functions = 0usize;
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
        let relative = path.strip_prefix(&root).unwrap_or(&path);
        let package = distribution(relative);
        for found in normalize_file(&parsed) {
            functions += 1;
            let form = found.form.simplify().canonical();
            if form.size() < MINIMUM_SHARED_SIZE {
                continue;
            }
            let start = found.start_byte as usize;
            let end = (found.end_byte as usize).min(source.len());
            groups.entry(form.to_string()).or_default().push(Site {
                package: package.clone(),
                display: relative.to_string_lossy().into_owned(),
                name: found.name.clone(),
                source: String::from_utf8_lossy(&source[start..end]).into_owned(),
                size: form.size(),
            });
        }
    }

    let across = |sites: &Vec<Site>, package: fn(&Site) -> &str| {
        let first = package(&sites[0]);
        sites.iter().any(|site| package(site) != first)
    };
    let shared: Vec<_> = groups
        .into_iter()
        .filter(|(_, sites)| across(sites, |site| site.display.split('/').next().unwrap_or("")))
        .collect();
    let unvendored: Vec<_> = shared
        .iter()
        .filter(|(_, sites)| across(sites, |site| site.package.as_str()))
        .collect();
    let renamed: Vec<_> = unvendored
        .iter()
        .filter(|(_, sites)| sites.iter().any(|site| site.name != sites[0].name))
        .collect();

    println!("functions                         {functions}");
    println!("forms shared across directories   {}", shared.len());
    println!("  once vendored copies are one    {}", unvendored.len());
    println!("  and the names differ            {}", renamed.len());

    let mut ranked = renamed;
    ranked.sort_by_key(|(_, sites)| std::cmp::Reverse(sites[0].size));
    for (_, sites) in ranked.iter().take(6) {
        println!("\n{}", "=".repeat(72));
        println!("form size {}", sites[0].size);
        let mut seen: Vec<&str> = Vec::new();
        for site in sites.iter() {
            if seen.contains(&site.package.as_str()) {
                continue;
            }
            seen.push(&site.package);
            println!("\n  -- {} :: {}", site.display, site.name);
            for line in site.source.lines().take(12) {
                println!("     {line}");
            }
        }
    }
    Ok(())
}

/// The library a path belongs to, seeing through vendoring.
///
/// `pip/_vendor/pygments/style.py` and `pygments/style.py` are one library, and
/// pairing them reports that pip vendors pygments rather than anything about
/// either.
fn distribution(relative: &Path) -> String {
    let parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts
        .iter()
        .position(|part| matches!(part.as_str(), "_vendor" | "vendored" | "_vendored"))
        .and_then(|at| parts.get(at + 1))
        .or_else(|| parts.first())
        .cloned()
        .unwrap_or_default()
}

fn collect(root: &Path, into: &mut Vec<PathBuf>) {
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
