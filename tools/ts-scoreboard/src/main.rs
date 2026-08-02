//! Scores derived behavior matches against the lint plugins' own rule tests.
//!
//! The plugins ship a labelled corpus nobody wrote for us: `invalid` cases are
//! annotated positives and `valid` cases are annotated NEGATIVES, which the
//! Rust side's clippy corpus does not provide at all.
//!
//! A case is credited only when a match NAMES THE API THE RULE ASKS FOR.
//! Requiring merely that something matched is not a metric — the Rust
//! scoreboard read 24/201 that way and it was fake.

use std::collections::BTreeMap;
use std::path::PathBuf;

use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime};
use infact_normalize::Form;
use infact_ts_normalize::{normalize_file, normalize_module};
use serde::Deserialize;

/// Receiver types observed by the TypeScript checker, per case file.
///
/// `xs.filter(p)[0]` is a search worth reporting when `xs` is an array and
/// nothing of the kind when it is a `Set` or someone's own type that spells a
/// method `filter`. The text is identical; only the type differs.
fn observed_receivers(cases: &[Case], typescript: Option<PathBuf>) -> BTreeMap<String, Vec<String>> {
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
        std::fs::write(root.join("src").join(format!("case_{index}.ts")), &case.code)
            .expect("case file");
    }
    let options = entl_ts_observe::Options {
        typescript: typescript.filter(|path| path.is_file()),
        node: None,
    };
    if !entl_ts_observe::available(&options) {
        eprintln!("no TypeScript observer on this machine; scoring without types");
        return BTreeMap::new();
    }
    match entl_ts_observe::observe(&root, &options) {
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

#[derive(Debug, Deserialize)]
struct Case {
    file: String,
    rule: String,
    apis: Vec<String>,
    kind: String,
    code: String,
}

struct Harness {
    catalog: ParserCatalog,
    runtime: ParserRuntime,
}

impl Harness {
    /// Parse one source, saying why rather than only that it did not happen.
    ///
    /// A harness that reports a parse failure as an absent file scores the case
    /// as "nothing matched", which is indistinguishable from a real negative
    /// and quietly flatters the number.
    fn parse(&self, language: &str, name: &str, source: String) -> Result<ParsedFile, String> {
        let path = PathBuf::from(name);
        let pack = self
            .catalog
            .resolve(language, &path)
            .ok_or_else(|| format!("no {language} parser pack for {}", path.display()))?
            .clone();
        self.runtime
            .load(pack)
            .map_err(|error| format!("loading the {language} parser: {error}"))?
            .parse(path.clone(), source.into_bytes())
            .map_err(|error| format!("parsing {}: {error}", path.display()))
    }
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

/// Where a run reads from, all overridable so this is not tied to one machine.
struct Paths {
    parser_packs: PathBuf,
    library: PathBuf,
    cases: PathBuf,
    typescript: Option<PathBuf>,
}

fn paths() -> Paths {
    let argument = |name: &str| {
        std::env::args()
            .position(|value| value == format!("--{name}"))
            .and_then(|at| std::env::args().nth(at + 1))
            .map(PathBuf::from)
    };
    // Same layout the other harnesses use: everything too large or too fetched
    // to vendor lives outside the repository, under <repo>/../measure.
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let measure = argument("measure")
        .or_else(|| std::env::var_os("INFACT_MEASURE").map(PathBuf::from))
        .unwrap_or_else(|| repo.join("../measure"));
    Paths {
        parser_packs: argument("parser-packs")
            .or_else(|| std::env::var_os("INFACT_PARSER_PATH").map(PathBuf::from))
            .unwrap_or_else(|| repo.join("../entl/parser-packs")),
        library: argument("library").unwrap_or_else(|| measure.join("spidermonkey")),
        cases: argument("cases").unwrap_or_else(|| measure.join("ts-lints/cases.json")),
        typescript: argument("typescript"),
    }
}

fn main() {
    let paths = paths();
    let harness = {
        let discovery = ParserCatalog::discover([paths.parser_packs.clone()]);
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        Harness { catalog: discovery.catalog, runtime: ParserRuntime::new().expect("runtime") }
    };

    // The library: every builtin SpiderMonkey self-hosts, derived here.
    let mut behaviors: Vec<(String, Form)> = Vec::new();
    for name in ["Array", "String", "Object", "Map"] {
        let path = paths.library.join(format!("{name}.js"));
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let file = harness
            .parse("javascript", &path.to_string_lossy(), source)
            .unwrap_or_else(|error| panic!("{error}"));
        for function in normalize_file(&file) {
            if function.damaged { continue; }
            let form = function.form.simplify().canonical();
            // The same gate the Rust pipeline applies. Without it a handful of
            // near-empty engine helpers match almost anything: on this corpus
            // `$ObjectProtoSetter` alone fired on 41 cases.
            if form.is_trivial()
                || !infact_rust_behaviors::is_comparable(&form)
                || !infact_rust_behaviors::is_reportable(&form)
            {
                continue;
            }
            behaviors.push((function.name, form));
        }
    }
    eprintln!("{} library behaviors derived", behaviors.len());

    let cases: Vec<Case> = serde_json::from_str(
        &std::fs::read_to_string(&paths.cases)
            .unwrap_or_else(|error| panic!("reading {}: {error}", paths.cases.display())),
    )
    .expect("decode cases");

    let receivers = observed_receivers(&cases, paths.typescript.clone());
    let use_types = !receivers.is_empty();
    eprintln!("receiver types observed for {} case files", receivers.len());

    #[derive(Default)]
    struct Tally { positives: u32, hit: u32, off_target: u32, negatives: u32, false_positive: u32, unparsed: u32 }
    let mut by_rule: BTreeMap<String, Tally> = BTreeMap::new();
    let mut fired: Vec<(String, String)> = Vec::new();

    for (index, case) in cases.iter().enumerate() {
        let tally = by_rule.entry(case.rule.clone()).or_default();
        // What the checker saw for this case, when it was asked.
        let heads = receivers.get(&format!("src/case_{index}.ts"));
        // the snippets are whole programs: statements at the top level plus
        // whatever functions they declare. Both have to be looked at.
        let file = match harness.parse("typescript", "case.ts", case.code.clone()) {
            Ok(file) => file,
            // a case that could not be read is not a case that matched nothing,
            // and the tally alone would not say which
            Err(error) => {
                eprintln!("case {index} ({}): {error}", case.rule);
                tally.unparsed += 1;
                continue;
            }
        };
        if file.tree.root_node().has_error() {
            tally.unparsed += 1;
            continue;
        }
        let mut named: Vec<&str> = Vec::new();
        let mut forms = vec![normalize_module(&file)];
        forms.extend(normalize_file(&file).into_iter().map(|f| f.form));
        for form in forms {
            let form = form.simplify().canonical();
            for (name, behavior) in &behaviors {
                if form.matches(behavior) || form.contains(behavior) {
                    named.push(name);
                }
            }
        }
        named.sort_unstable();
        named.dedup();
        // An Array behavior is only a finding on something that is an array.
        //
        // A receiver the checker typed as `any` says nothing — the snippet
        // simply never declared it — and declining on that would turn "not
        // looked at" into "looked and it is not an array", which is the one
        // answer a consumer cannot see through. Only a KNOWN, non-array
        // receiver declines the match.
        if use_types
            && !named.is_empty()
            && let Some(heads) = heads
            && heads.iter().any(|head| known(head))
            && !heads.iter().any(|head| array_like(head))
        {
            named.clear();
        }
        for name in &named { fired.push((case.kind.clone(), (*name).to_owned())); }
        let on_target = named.iter().any(|name| case.apis.iter().any(|api| api == name));
        if case.kind == "invalid" {
            tally.positives += 1;
            if on_target { tally.hit += 1; }
            else if !named.is_empty() { tally.off_target += 1; }
        } else {
            tally.negatives += 1;
            // a finding on code the rule calls correct is a false positive
            if !named.is_empty() {
                tally.false_positive += 1;
                eprintln!("FP [{}] {:?} <- {}", case.rule, named,
                    case.code.replace('\n', " ").trim().chars().take(88).collect::<String>());
            }
        }
    }

    println!("\n=== which behaviors fire, by case kind ===");
    let mut hist: BTreeMap<(String, String), u32> = BTreeMap::new();
    for (kind, name) in &fired { *hist.entry((kind.clone(), name.clone())).or_default() += 1; }
    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by_key(|((_, _), n)| std::cmp::Reverse(*n));
    for ((kind, name), n) in rows.iter().take(14) { println!("  {kind:<8} {name:<28} {n}"); }

    println!("\n{:<38} {:>9} {:>5} {:>10} {:>9} {:>6} {:>8}",
        "rule", "positives", "found", "off-target", "negatives", "false", "unparsed");
    let (mut tp, mut th, mut tn, mut tf) = (0, 0, 0, 0);
    for (rule, t) in &by_rule {
        println!("{:<38} {:>9} {:>5} {:>10} {:>9} {:>6} {:>8}",
            rule, t.positives, t.hit, t.off_target, t.negatives, t.false_positive, t.unparsed);
        tp += t.positives; th += t.hit; tn += t.negatives; tf += t.false_positive;
    }
    println!("{:<38} {:>9} {:>5} {:>10} {:>9} {:>6}", "TOTAL", tp, th, "", tn, tf);
    if tp > 0 { println!("\nrecall  {:.1}%  ({th}/{tp})", 100.0 * f64::from(th) / f64::from(tp)); }
    if tn > 0 { println!("false positives on annotated-correct code: {tf}/{tn}"); }
}
