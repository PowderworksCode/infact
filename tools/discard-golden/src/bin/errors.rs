//! Is the discard analyzer's SCAFFOLDING expressible as queries?
//!
//! Everything below is language-neutral: it names no Rust node kind. Callables,
//! impls, modules, attributes and call edges arrive from `callables.scm`;
//! discards from `discards.scm`; and the association between them is decided by
//! byte-range containment, which is a property of trees, not of Rust.
//!
//! The result is diffed against `infact_errors::analyze_file`, the real
//! implementation, over real source files.
//!
//! This is the experiment that motivated the port, kept because it is the only
//! executable statement of the hypothesis. `infact-errors` now works this way,
//! so a clean run here is two implementations agreeing rather than a proposal
//! being tested. It still earns its keep: it reads the queries straight from
//! the pack, so a change to `discards.scm` that breaks the correspondence shows
//! up as a diff rather than as silence.
//!
//! Usage: cargo run --bin errors -- <file.rs> ...
//!        DUMP=1 cargo run --bin errors -- <file.rs>   print the parse tree

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use entl_tree_sitter::{ParsedFile, ParserPack, ParserRuntime};
use infact_core::{Certainty, Containment, DiscardForm};
use tree_sitter::Node;

/// The live Rust pack in the sibling entl checkout — not a vendored copy, so
/// this diffs against the queries that actually ship. Override with `PACK_DIR`.
const PACK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../entl/parser-packs/rust"
);

/// Per-language data that stays in `parser.toml`, not in a query.
/// These are facts about a language's stdlib, not shapes in its grammar.
const FALLIBLE_TYPES: &[&str] = &["Result"];
const OPTIONAL_TYPES: &[&str] = &["Option"];
const TEST_MARKER: &str = "test";
const TEST_MODULE_MARKER: &str = "cfg(test)";

struct Callable {
    name: String,
    item: (usize, usize),
    body: (usize, usize),
    containment: Containment,
}

fn range(node: Node<'_>) -> (usize, usize) {
    (node.start_byte(), node.end_byte())
}

fn contains(outer: (usize, usize), point: usize) -> bool {
    outer.0 <= point && point < outer.1
}

fn text(file: &ParsedFile, node: Node<'_>) -> String {
    String::from_utf8_lossy(&file.source[node.byte_range()]).into_owned()
}

/// `{module}::{implementation}::{name}`, assembled positionally.
fn repository_module(path: &Path) -> String {
    let mut module = path
        .with_extension("")
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    if matches!(
        module.last().map(String::as_str),
        Some("lib" | "main" | "mod")
    ) {
        module.pop();
    }
    if module.is_empty() {
        "crate".to_owned()
    } else {
        module.join("::")
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pack = Arc::new(ParserPack::load(
        std::env::var("PACK_DIR").unwrap_or_else(|_| PACK.to_owned()),
    )?);
    let runtime = ParserRuntime::new()?;
    let parser = runtime.load(pack)?;

    let (mut agree, mut differ, mut files) = (0usize, 0usize, 0usize);
    for target in std::env::args().skip(1) {
        let source = std::fs::read(&target)?;
        let path = Path::new(&target)
            .file_name()
            .map(|n| Path::new(n).to_path_buf())
            .unwrap_or_default();
        let file = parser.parse(path.clone(), source)?;
        if std::env::var("DUMP").is_ok() {
            let mut c = file.tree.walk();
            let (mut d, mut down) = (0usize, true);
            loop {
                if down && c.node().is_named() {
                    let n = c.node();
                    let f = c.field_name().map(|f| format!("{f}: ")).unwrap_or_default();
                    let t = if n.child_count() == 0 {
                        format!(
                            "  {:?}",
                            String::from_utf8_lossy(&file.source[n.byte_range()])
                        )
                    } else {
                        String::new()
                    };
                    println!("{:i$}{f}({}){t}", "", n.kind(), i = d * 2);
                }
                if down && c.goto_first_child() {
                    d += 1;
                } else if c.goto_next_sibling() {
                    down = true;
                } else if c.goto_parent() {
                    d -= 1;
                    down = false;
                } else {
                    break;
                }
            }
            continue;
        }
        files += 1;

        // --- scaffolding, entirely from callables.scm -----------------------
        if std::env::var("PROBE").is_ok() {
            for m in parser.matches("probe", &file)? {
                let n = m.capture("site").unwrap();
                println!(
                    "  MATCH {}",
                    String::from_utf8_lossy(&file.source[n.byte_range()])
                );
            }
            continue;
        }
        let scaffold = parser.matches("callables", &file)?;
        let mut callables: Vec<Callable> = Vec::new();
        let mut returns: BTreeMap<(usize, usize), String> = BTreeMap::new();
        for matched in &scaffold {
            if let (Some(body), Some(ret)) = (
                matched.capture("callable.with-return"),
                matched.capture("callable.return"),
            ) {
                returns.insert(range(body), text(&file, ret));
            }
        }
        for matched in &scaffold {
            let (Some(item), Some(name), Some(body)) = (
                matched.capture("callable.item"),
                matched.capture("callable.name"),
                matched.capture("callable.body"),
            ) else {
                continue;
            };
            let containment = match returns.get(&range(item)) {
                None => Containment::Infallible,
                Some(declared) => {
                    let leading = declared.split('<').next().unwrap_or(declared);
                    if FALLIBLE_TYPES.iter().any(|t| leading.contains(t)) {
                        Containment::Fallible
                    } else if OPTIONAL_TYPES.iter().any(|t| leading.contains(t)) {
                        Containment::Optional
                    } else {
                        Containment::Infallible
                    }
                }
            };
            callables.push(Callable {
                name: text(&file, name),
                item: range(item),
                body: range(body),
                containment,
            });
        }
        let impls: Vec<(String, (usize, usize))> = scaffold
            .iter()
            .filter_map(|m| {
                Some((
                    text(&file, m.capture("impl.type")?),
                    range(m.capture("impl.item")?),
                ))
            })
            .collect();
        // An attribute applies to the item that follows it, so a test marker is
        // recorded as the region from the attribute to the end of that item.
        let test_regions: Vec<(usize, usize)> = scaffold
            .iter()
            .filter_map(|m| {
                let item = m.capture("attribute.item")?;
                let body =
                    text(&file, m.capture("attribute.text")?).replace(char::is_whitespace, "");
                let marks = body.contains(TEST_MODULE_MARKER) || body.contains(TEST_MARKER);
                let next = item.next_named_sibling()?;
                marks.then(|| (item.start_byte(), next.end_byte()))
            })
            .collect();

        let module = repository_module(&path);
        let scope_of = |point: usize| -> Option<(String, Containment, bool)> {
            let callable = callables
                .iter()
                .filter(|c| contains(c.body, point))
                .min_by_key(|c| c.body.1 - c.body.0)?;
            let implementation = impls
                .iter()
                .filter(|(_, r)| contains(*r, callable.item.0))
                .min_by_key(|(_, r)| r.1 - r.0)
                .map(|(name, _)| name.split('<').next().unwrap_or(name).trim().to_owned());
            let path = match implementation {
                Some(implementation) => format!("{module}::{implementation}::{}", callable.name),
                None => format!("{module}::{}", callable.name),
            };
            let in_test = test_regions.iter().any(|r| contains(*r, point));
            Some((path, callable.containment, in_test))
        };

        // --- discards, from discards.scm ------------------------------------
        let mut mine: Vec<(String, DiscardForm, String, Containment, bool)> = Vec::new();
        for matched in parser.matches("discards", &file)? {
            let (form, node) = if let Some(n) = matched.capture("discard.let-underscore") {
                (DiscardForm::LetUnderscore, n)
            } else if let Some(n) = matched.capture("discard.ok-binding") {
                (DiscardForm::OkBinding, n)
            } else if let Some(n) = matched.capture("discard.err-arm") {
                if matched.has("discard.err-arm.bind") {
                    continue;
                }
                (DiscardForm::ErrArm, n)
            } else {
                continue;
            };
            let Some((callable, containment, in_test)) = scope_of(node.start_byte()) else {
                continue;
            };
            mine.push((
                callable,
                form,
                format!("{}", node.start_byte()),
                containment,
                in_test,
            ));
        }

        // --- the oracle ------------------------------------------------------
        let real = infact_errors::analyze_file(&parser, &file)?;
        let real_keyed: BTreeMap<String, _> = real
            .iter()
            .filter(|d| {
                matches!(
                    d.form,
                    DiscardForm::LetUnderscore | DiscardForm::OkBinding | DiscardForm::ErrArm
                ) && d.certainty == Certainty::Certain
            })
            .map(|d| {
                (
                    format!("{:?}@{}", d.form, d.span.start_byte.unwrap_or_default()),
                    d,
                )
            })
            .collect();
        let mine_keyed: BTreeMap<String, _> = mine
            .iter()
            .map(|(c, f, b, ct, t)| (format!("{f:?}@{b}"), (c, ct, t)))
            .collect();

        for (key, expected) in &real_keyed {
            match mine_keyed.get(key) {
                Some((callable, containment, in_test))
                    if **callable == expected.callable
                        && **containment == expected.containment
                        && **in_test == expected.in_test =>
                {
                    agree += 1;
                }
                Some((callable, containment, in_test)) => {
                    differ += 1;
                    println!(
                        "DIFFER {target} {key}\n  real: {} / {:?} / test={}\n  mine: {callable} / {containment:?} / test={in_test}",
                        expected.callable, expected.containment, expected.in_test
                    );
                }
                None => {
                    differ += 1;
                    println!("MISSING {target} {key}  real: {}", expected.callable);
                }
            }
        }
        for key in mine_keyed.keys() {
            if !real_keyed.contains_key(key) {
                differ += 1;
                println!("EXTRA {target} {key}");
            }
        }
    }
    eprintln!("\n{files} files: {agree} agree, {differ} differ");
    Ok(())
}
