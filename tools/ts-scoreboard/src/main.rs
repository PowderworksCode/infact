//! Scores derived behavior matches against the lint plugins' own rule tests.
//!
//! The plugins ship a labelled corpus nobody wrote for us: `invalid` cases are
//! annotated positives and `valid` cases are annotated NEGATIVES, which the
//! Rust side's clippy corpus does not provide at all.
//!
//! A case is credited only when a match NAMES THE API THE RULE ASKS FOR.
//! Requiring merely that something matched is not a metric — the Rust
//! scoreboard read 24/201 that way and it was fake.
//!
//! This runs the shipping pipeline rather than a copy of it: `derive_library`
//! reads the engine's self-hosted builtins, and `analyze_repository` matches the
//! corpus against them. An earlier version of this file re-implemented both in
//! miniature, which measured the laws but not the crate anyone would use.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use entl_tree_sitter::ParserCatalog;
use infact_core::LibraryTarget;
use serde::Deserialize;

/// Receiver types observed by the TypeScript checker, per case file.
///
/// `xs.filter(p)[0]` is a search worth reporting when `xs` is an array and
/// nothing of the kind when it is a `Set` or someone's own type that spells a
/// method `filter`. The text is identical; only the type differs.
fn observed_receivers(root: &Path, typescript: Option<PathBuf>) -> BTreeMap<String, Vec<String>> {
    let options = entl_ts_observe::Options {
        typescript: typescript.filter(|path| path.is_file()),
        node: None,
    };
    if !entl_ts_observe::available(&options) {
        eprintln!("no TypeScript observer on this machine; scoring without types");
        return BTreeMap::new();
    }
    match entl_ts_observe::observe(root, &options) {
        Ok(observed) => {
            let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for observed in &observed.types {
                by_file
                    .entry(observed.span.path.to_string_lossy().into_owned())
                    .or_default()
                    .push(observed.type_ref.head.clone());
            }
            by_file
        }
        Err(error) => {
            eprintln!("observing the corpus: {error}");
            BTreeMap::new()
        }
    }
}

/// Write every case as its own file, which is what both halves read.
///
/// The checker needs a project on disk, and so does `analyze_repository`. One
/// corpus serves both, so what is typed and what is matched cannot drift apart.
fn write_corpus(cases: &[Case]) -> PathBuf {
    let root = std::env::temp_dir().join("ts-scoreboard-corpus");
    if let Err(error) = std::fs::remove_dir_all(&root)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("clearing {}: {error}", root.display());
    }
    std::fs::create_dir_all(root.join("src")).expect("corpus directory");
    std::fs::write(
        root.join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":false,"target":"ES2022","module":"ESNext","moduleResolution":"bundler","types":[],"noEmit":true},"include":["src/**/*.ts"]}"#,
    )
    .expect("tsconfig");
    for (index, case) in cases.iter().enumerate() {
        std::fs::write(
            root.join("src").join(format!("case_{index}.ts")),
            &case.code,
        )
        .expect("case file");
    }
    root
}

#[derive(Debug, Deserialize)]
struct Case {
    rule: String,
    apis: Vec<String>,
    kind: String,
    code: String,
}

/// Whether the checker actually established anything about this type.
fn known(head: &str) -> bool {
    !matches!(head, "any" | "unknown" | "error" | "never" | "(anonymous)")
}

/// Whether a type is one the array behaviors apply to.
///
/// The typed arrays carry the same `filter`/`find` surface as `Array` and a
/// reimplementation of a search over one is the same finding.
fn array_like(head: &str) -> bool {
    head == "Array" || head.ends_with("Array")
}

/// The case a reported span belongs to.
///
/// Every file in the corpus is named `case_<n>.ts` by `write_corpus`, so a name
/// that does not read as one is not a case and there is no failure to report.
fn case_index(path: &Path) -> Option<usize> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("case_")?
        .parse()
        .ok() // straitjacket-allow:error-discard — a name that is not `case_<n>` names no case
}

/// Where a run reads from, all overridable so this is not tied to one machine.
struct Paths {
    parser_packs: PathBuf,
    library: PathBuf,
    cases: PathBuf,
    typescript: Option<PathBuf>,
    version: String,
}

fn paths() -> Paths {
    let argument = |name: &str| {
        std::env::args()
            .position(|value| value == format!("--{name}"))
            .and_then(|at| std::env::args().nth(at + 1))
    };
    // Same layout the other harnesses use: everything too large or too fetched
    // to vendor lives outside the repository, under <repo>/../measure.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let measure = argument("measure")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("INFACT_MEASURE").map(PathBuf::from))
        .unwrap_or_else(|| repo.join("../measure"));
    Paths {
        parser_packs: argument("parser-packs")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("INFACT_PARSER_PATH").map(PathBuf::from))
            .unwrap_or_else(|| repo.join("../entl/parser-packs")),
        library: argument("library")
            .map(PathBuf::from)
            .unwrap_or_else(|| measure.join("spidermonkey")),
        cases: argument("cases")
            .map(PathBuf::from)
            .unwrap_or_else(|| measure.join("ts-lints/cases.json")),
        typescript: argument("typescript").map(PathBuf::from),
        // The source is fetched rather than released, so there is no version to
        // quote. The catalog's digest is what actually identifies it, and it is
        // recorded there whatever this says.
        version: argument("library-version").unwrap_or_else(|| "local".to_owned()),
    }
}

fn main() {
    let paths = paths();
    let parsers = {
        let discovery = ParserCatalog::discover([paths.parser_packs.clone()]);
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        discovery.catalog
    };

    // The library: every builtin the engine self-hosts, derived by the crate
    // that ships rather than by this file.
    let library = infact_ts_behaviors::derive_library(
        &paths.library,
        &parsers,
        "ecmascript",
        &paths.version,
    )
    .unwrap_or_else(|error| panic!("deriving {}: {error}", paths.library.display()));
    let reportable = library
        .behaviors
        .iter()
        .filter(|behavior| behavior.program.is_reportable())
        .count();
    eprintln!(
        "{} callables, {} behaviors derived, {reportable} of them reportable",
        library.catalog.callables.len(),
        library.behaviors.len(),
    );
    for (reason, count) in &library.skipped {
        eprintln!("  {count:>5}  {reason}");
    }
    for unparsed in &library.unparsed {
        eprintln!("  UNREAD  {unparsed}");
    }
    for name in &library.damaged {
        eprintln!("  DAMAGED {name}");
    }
    eprintln!(
        "  reportable: {}",
        library
            .behaviors
            .iter()
            .filter(|behavior| behavior.program.is_reportable())
            .map(|behavior| behavior.callable_path.rsplit("::").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let cases: Vec<Case> = serde_json::from_str(
        &std::fs::read_to_string(&paths.cases)
            .unwrap_or_else(|error| panic!("reading {}: {error}", paths.cases.display())),
    )
    .expect("decode cases");

    let corpus = write_corpus(&cases);
    let receivers = observed_receivers(&corpus, paths.typescript.clone());
    let use_types = !receivers.is_empty();
    eprintln!("receiver types observed for {} case files", receivers.len());

    let report = infact_ts_behaviors::analyze_repository(
        &corpus,
        &parsers,
        std::slice::from_ref(&library.catalog),
        &library.behaviors,
    )
    .expect("analyze the corpus");
    for diagnostic in &report.diagnostics {
        eprintln!("UNPARSED {}: {}", diagnostic.path.display(), diagnostic.message);
    }

    // What each case was told it reimplements.
    let mut named: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for matched in &report.matches {
        let Some(index) = case_index(&matched.value.span.path) else {
            continue;
        };
        let LibraryTarget::Callable { path, .. } = &matched.value.target else {
            continue;
        };
        let leaf = path.rsplit("::").next().unwrap_or(path).to_owned();
        let entry = named.entry(index).or_default();
        if !entry.contains(&leaf) {
            entry.push(leaf);
        }
    }
    for names in named.values_mut() {
        names.sort_unstable();
    }

    #[derive(Default)]
    struct Tally {
        positives: u32,
        hit: u32,
        off_target: u32,
        negatives: u32,
        false_positive: u32,
    }
    let mut by_rule: BTreeMap<String, Tally> = BTreeMap::new();
    let mut fired: Vec<(String, String)> = Vec::new();

    for (index, case) in cases.iter().enumerate() {
        let tally = by_rule.entry(case.rule.clone()).or_default();
        let mut names = named.get(&index).cloned().unwrap_or_default();
        // An Array behavior is only a finding on something that is an array.
        //
        // A receiver the checker typed as `any` says nothing — the snippet
        // simply never declared it — and declining on that would turn "not
        // looked at" into "looked and it is not an array", which is the one
        // answer a consumer cannot see through. Only a KNOWN, non-array
        // receiver declines the match.
        if use_types
            && !names.is_empty()
            && let Some(heads) = receivers.get(&format!("src/case_{index}.ts"))
            && heads.iter().any(|head| known(head))
            && !heads.iter().any(|head| array_like(head))
        {
            names.clear();
        }
        for name in &names {
            fired.push((case.kind.clone(), name.clone()));
        }
        let on_target = names
            .iter()
            .any(|name| case.apis.iter().any(|api| api == name));
        if case.kind == "invalid" {
            tally.positives += 1;
            if on_target {
                tally.hit += 1;
            } else if !names.is_empty() {
                tally.off_target += 1;
            }
        } else {
            tally.negatives += 1;
            // a finding on code the rule calls correct is a false positive
            if !names.is_empty() {
                tally.false_positive += 1;
                eprintln!(
                    "FP [{}] {:?} <- {}",
                    case.rule,
                    names,
                    case.code
                        .replace('\n', " ")
                        .trim()
                        .chars()
                        .take(88)
                        .collect::<String>()
                );
            }
        }
    }

    println!("\n=== which behaviors fire, by case kind ===");
    let mut hist: BTreeMap<(String, String), u32> = BTreeMap::new();
    for (kind, name) in &fired {
        *hist.entry((kind.clone(), name.clone())).or_default() += 1;
    }
    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by_key(|((_, _), n)| std::cmp::Reverse(*n));
    for ((kind, name), n) in rows.iter().take(14) {
        println!("  {kind:<8} {name:<28} {n}");
    }

    println!(
        "\n{:<38} {:>9} {:>5} {:>10} {:>9} {:>6}",
        "rule", "positives", "found", "off-target", "negatives", "false"
    );
    let (mut tp, mut th, mut tn, mut tf) = (0, 0, 0, 0);
    for (rule, t) in &by_rule {
        println!(
            "{:<38} {:>9} {:>5} {:>10} {:>9} {:>6}",
            rule, t.positives, t.hit, t.off_target, t.negatives, t.false_positive
        );
        tp += t.positives;
        th += t.hit;
        tn += t.negatives;
        tf += t.false_positive;
    }
    println!(
        "{:<38} {:>9} {:>5} {:>10} {:>9} {:>6}",
        "TOTAL", tp, th, "", tn, tf
    );
    if tp > 0 {
        println!(
            "\nrecall  {:.1}%  ({th}/{tp})",
            100.0 * f64::from(th) / f64::from(tp)
        );
    }
    if tn > 0 {
        println!("false positives on annotated-correct code: {tf}/{tn}");
    }
}
