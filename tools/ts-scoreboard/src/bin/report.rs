//! Writes a reuse report for a TypeScript project, with links back to source.
//!
//! Three questions, each answered by a different reading of the same normalized
//! behavior form:
//!
//!   duplication  the same behavior implemented in two places, whatever it is
//!                called. This is what a name search and a token diff both
//!                miss, because the names differ and the tokens do too.
//!   abstraction  a shape with internal structure repeated across many files.
//!                No single site is worth extracting; the count is the finding.
//!   idiom        a small shape repeated everywhere. Reported separately and
//!                deliberately not as a defect: `xs.length > 0` is how the
//!                language is written, not a problem to fix.
//!
//! The three want opposite thresholds. Big-and-repeated finds duplication;
//! small-and-everywhere finds vocabulary. One setting can only ever see one of
//! them, which is why this makes two passes and labels them apart.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use entl_tree_sitter::{LoadedParser, ParserCatalog, ParserRuntime};
use infact_normalize::Form;
use infact_ts_normalize::normalize_file;

/// Where a shape has to sit to count as a candidate abstraction.
const ABSTRACTION_MIN_NODES: u32 = 7;
const ABSTRACTION_MIN_FILES: usize = 4;

/// And where a shape is small enough to be vocabulary rather than logic.
const IDIOM_MIN_NODES: u32 = 4;
const IDIOM_MIN_FILES: usize = 10;

/// A shape this large is a particular piece of work, not a pattern.
const LARGEST_SHAPE: u32 = 40;

/// Trees that are deliberately copies of one another.
///
/// A fork is not a finding. Reporting one is noise, and worse, it buries the
/// findings that are not explained by it. Given a set of mirrored prefixes,
/// every path under any of them collapses to one location, so a group living
/// entirely inside the fork counts as a single place and drops out — while a
/// group that also appears somewhere else keeps its second location and stays.
/// Set with REPORT_MIRRORS, prefixes separated by commas.
fn mirrors() -> Vec<String> {
    std::env::var("REPORT_MIRRORS")
        .unwrap_or_default()
        .split(',')
        .map(|prefix| prefix.trim().to_owned())
        .filter(|prefix| !prefix.is_empty())
        .collect()
}

/// Where a path counts as being, once mirrored trees are folded together.
fn location<'a>(path: &'a str, mirrors: &[String]) -> &'a str {
    for prefix in mirrors {
        if path.starts_with(prefix.as_str()) {
            return "«mirrored»";
        }
    }
    path
}

struct Repository {
    /// `owner/name` on GitHub, when the remote is one.
    slug: Option<String>,
    commit: Option<String>,
    dirty: bool,
}

impl Repository {
    fn discover(root: PathBuf) -> Self {
        // A tree with no git, or no remote, has no permalinks. That is an
        // answer rather than a failure, so it is the `None` this returns —
        // and the report says plainly that it fell back to plain paths.
        let git = |arguments: &[&str]| match Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(arguments)
            .output()
        {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            }
            _ => None,
        };
        let slug = git(&["remote", "get-url", "origin"]).and_then(|url| {
            let trimmed = url.trim_end_matches(".git");
            trimmed
                .rsplit_once("github.com")
                .map(|(_, path)| path.trim_start_matches([':', '/']).to_owned())
        });
        Self {
            slug,
            commit: git(&["rev-parse", "HEAD"]),
            dirty: git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty()),
        }
    }

    /// A permalink to one line, or plain text when this is not a GitHub tree.
    ///
    /// Pinned to the commit, because a report that drifts against `main` is a
    /// report nobody can check later.
    fn link(&self, path: &str, line: u32) -> String {
        match (&self.slug, &self.commit) {
            (Some(slug), Some(commit)) => {
                format!("[`{path}:{line}`](https://github.com/{slug}/blob/{commit}/{path}#L{line})")
            }
            _ => format!("`{path}:{line}`"),
        }
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, unread: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // a directory nobody could read is not a directory with nothing in it
        Err(error) => {
            unread.push(format!("{}: {error}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "node_modules" || name.starts_with('.') || name == "dist" {
            continue;
        }
        if path.is_dir() {
            collect(&path, out, unread);
        } else if matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("ts" | "tsx")
        ) && !name.ends_with(".d.ts")
        {
            out.push(path);
        }
    }
}

/// The concrete names a form insists on.
///
/// A pattern only matches a subject containing every name it states outright,
/// so a set test rules out almost everything before any tree is walked. On this
/// corpus it removed 99.5% of the candidate pairs.
fn anchors(form: &Form, into: &mut BTreeSet<String>) {
    match form {
        Form::Method { name, .. } => {
            into.insert(format!("m:{name}"));
        }
        Form::Constant(value) => {
            into.insert(format!("c:{value}"));
        }
        Form::Path(path) => {
            into.insert(format!("p:{path}"));
        }
        Form::Construct(name) => {
            into.insert(format!("n:{name}"));
        }
        Form::Variant { name, .. } => {
            into.insert(format!("v:{name}"));
        }
        Form::Field { name, .. } => {
            into.insert(format!("f:{name}"));
        }
        Form::Binary { operator, .. } => {
            into.insert(format!("o:{operator}"));
        }
        _ => {}
    }
    for child in form.children() {
        anchors(child, into);
    }
}

/// Source a library ships that is worth reading.
///
/// A bundled or minified file is one enormous line and describes the optimizer
/// rather than the author; nothing derived from it resembles code anyone wrote.
fn is_readable_source(path: &Path, unread: &mut Vec<String>) -> bool {
    let source = match std::fs::read(path) {
        Ok(source) => source,
        Err(error) => {
            unread.push(format!("{}: {error}", path.display()));
            return false;
        }
    };
    source.len() < 2_000_000
        && source
            .split(|byte| *byte == b'\n')
            .all(|line| line.len() < 400)
}

fn collect_library(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, unread: &mut Vec<String>) {
    if depth > 8 {
        unread.push(format!("{}: nested deeper than this reads", dir.display()));
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            unread.push(format!("{}: {error}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "test" || name == "tests" {
            continue;
        }
        if path.is_dir() {
            collect_library(&path, out, depth + 1, unread);
        } else if matches!(
            path.extension().and_then(|x| x.to_str()),
            Some("js" | "mjs" | "cjs" | "ts")
        ) && !name.ends_with(".d.ts")
            && !name.contains(".min.")
        {
            out.push(path);
        }
    }
}

fn subforms<'a>(form: &'a Form, out: &mut Vec<&'a Form>) {
    out.push(form);
    for child in form.children() {
        subforms(child, out);
    }
}

#[derive(Default)]
struct Shape {
    size: u32,
    sites: BTreeSet<(String, u32, String)>,
    files: BTreeSet<String>,
}

/// One normalized function, kept so both passes read the same parse.
struct Callable {
    file: String,
    line: u32,
    name: String,
    form: Form,
    /// Where each step of `form` was written, so a match inside a large
    /// function can be reported at the statement rather than at the function.
    statements: Vec<infact_ts_normalize::StatementSpan>,
    located: Vec<infact_ts_normalize::LocatedForm>,
}

impl Callable {
    /// Every line a match on `pattern` sits at.
    ///
    /// A function that does the same thing four times has four findings. For
    /// each, the innermost statement whose own form carries the behavior is the
    /// place to point — a match three levels inside a four-hundred-line
    /// function should not be reported at the function. Where nothing can be
    /// placed that precisely it falls back to the top-level step and then to
    /// the function, so a placement is never more confident than it has earned.
    fn lines_of(&self, pattern: &Form) -> Vec<u32> {
        let mut carrying = self
            .located
            .iter()
            .filter(|located| {
                let form = located.form.simplify().canonical();
                form.matches(pattern) || form.contains(pattern)
            })
            .collect::<Vec<_>>();
        // an outer statement contains an inner one, and reporting both would
        // count one occurrence twice; keep only the innermost of each nest
        carrying.sort_by_key(|located| std::cmp::Reverse(located.depth));
        let mut lines: Vec<u32> = Vec::new();
        let mut covered: Vec<(u64, u64)> = Vec::new();
        for located in carrying {
            let extent = (located.span.start_byte, located.span.end_byte);
            // Skip a statement that merely wraps one already reported. The
            // callback a behavior sits inside is not where the behavior is.
            if covered
                .iter()
                .any(|(start, end)| extent.0 <= *start && extent.1 >= *end)
            {
                continue;
            }
            covered.push(extent);
            lines.push(located.span.start_line);
        }
        if !lines.is_empty() {
            lines.sort_unstable();
            lines.dedup();
            return lines;
        }
        vec![
            self.form
                .locate_all(pattern)
                .first()
                .and_then(|steps| self.statements.get(steps.start))
                .map_or(self.line, |span| span.start_line),
        ]
    }
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let root = PathBuf::from(arguments.next().expect("usage: report <project> [out.md]"));
    let out = arguments
        .next()
        .map_or_else(|| root.join("reuse-report.md"), PathBuf::from);
    let prefix = std::env::var("REPORT_PREFIX").unwrap_or_default();

    let mirrors = mirrors();
    let repository = Repository::discover(root.clone());
    let discovery = ParserCatalog::discover([PathBuf::from(
        std::env::var("INFACT_PARSER_PATH")
            .unwrap_or_else(|_| "/home/exedev/powderworks-ts/entl/parser-packs".to_owned()),
    )]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let runtime = ParserRuntime::new().expect("runtime");
    let parser: LoadedParser = {
        let pack = discovery
            .catalog
            .resolve("typescript", Path::new("x.ts"))
            .expect("a typescript parser pack")
            .clone();
        runtime.load(pack).expect("load the parser")
    };

    let mut unread: Vec<String> = Vec::new();
    let mut files = Vec::new();
    collect(&root, &mut files, &mut unread);
    files.sort();

    // --- read everything once
    let mut callables = Vec::new();
    let mut damaged = 0usize;
    for path in &files {
        let source = match std::fs::read(path) {
            Ok(source) => source,
            Err(error) => {
                unread.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let file = match parser.parse(path.clone(), source) {
            Ok(file) => file,
            Err(error) => {
                unread.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let relative = format!(
            "{prefix}{}",
            path.strip_prefix(&root).unwrap_or(path).to_string_lossy()
        );
        for function in normalize_file(&file) {
            if function.damaged {
                damaged += 1;
                continue;
            }
            callables.push(Callable {
                file: relative.clone(),
                line: function.start_line,
                name: function.name,
                form: function.form.simplify(),
                statements: function.statements,
                located: function.located,
            });
        }
    }

    // --- shapes, gathered once and split by threshold afterwards
    let mut shapes: BTreeMap<String, Shape> = BTreeMap::new();
    for callable in &callables {
        let mut found = Vec::new();
        subforms(&callable.form, &mut found);
        let mut here = BTreeMap::new();
        for candidate in found {
            let size = candidate.size();
            if size < IDIOM_MIN_NODES || size > LARGEST_SHAPE || candidate.is_trivial() {
                continue;
            }
            here.insert(candidate.canonical().to_string(), size);
        }
        for (shape, size) in here {
            let entry = shapes.entry(shape).or_default();
            entry.size = size;
            entry
                .sites
                .insert((callable.file.clone(), callable.line, callable.name.clone()));
            entry
                .files
                .insert(location(&callable.file, &mirrors).to_owned());
        }
    }

    // --- duplication: two callables reducing to one reportable form
    let mut by_form: BTreeMap<String, Vec<&Callable>> = BTreeMap::new();
    for callable in &callables {
        let canonical = callable.form.canonical();
        if !canonical.is_reportable() {
            continue;
        }
        by_form
            .entry(canonical.to_string())
            .or_default()
            .push(callable);
    }
    let mut suppressed_duplication = 0usize;
    let mut duplication = by_form
        .into_iter()
        .filter(|(_, group)| {
            let places = group
                .iter()
                .map(|c| location(&c.file, &mirrors))
                .collect::<BTreeSet<_>>();
            if group.len() > 1 && places.len() < 2 {
                suppressed_duplication += 1;
            }
            group.len() > 1 && places.len() > 1
        })
        .collect::<Vec<_>>();
    duplication.sort_by_key(|(form, group)| {
        (
            std::cmp::Reverse(group.len()),
            std::cmp::Reverse(group.iter().map(|c| &c.file).collect::<BTreeSet<_>>().len()),
            form.clone(),
        )
    });

    let ranked = |min_nodes: u32, min_files: usize, structural: bool| {
        let mut chosen = shapes
            .iter()
            .filter(|(_, shape)| {
                shape.size >= min_nodes
                    && shape.files.len() >= min_files
                    && shape.size <= LARGEST_SHAPE
            })
            .filter(|(form, _)| {
                // an abstraction has internal structure; a bare comparison is
                // how the language is written
                let has_structure = form.contains("(branch")
                    || form.contains("(traverse")
                    || form.contains("(sift")
                    || form.contains("(retain")
                    || form.contains("(transform")
                    || form.contains("(method")
                    || form.contains("(call");
                has_structure == structural
            })
            .collect::<Vec<_>>();
        // Sorted by how many places the shape is actually written. That is the
        // argument for doing something about it, and it is the number a reader
        // wants to compare rows on.
        chosen.sort_by_key(|(form, shape)| {
            (
                std::cmp::Reverse(shape.sites.len()),
                std::cmp::Reverse(shape.files.len()),
                (*form).clone(),
            )
        });
        chosen
    };

    // --- what a library already does
    //
    // Derived from the library's own source, so this names no API by hand: if
    // a dependency implements it and the project implements it again, that is
    // the finding, whatever either of them calls it.
    let mut library: Vec<(String, String, Form, BTreeSet<String>)> = Vec::new();
    // Where the time goes while reading a dependency tree, measured rather than
    // guessed: rejecting bundles reads every byte, and that is easy to
    // underestimate next to parsing.
    // One parser per language for the whole run. Loading one costs the better
    // part of a second; doing it per file was 99% of this loop's time.
    let mut parsers: BTreeMap<&str, LoadedParser> = BTreeMap::new();
    for (language, probe) in [("typescript", "x.ts"), ("javascript", "x.js")] {
        let Some(pack) = discovery.catalog.resolve(language, Path::new(probe)) else {
            // a language with no parser reads as a language with no code in it,
            // so it is recorded rather than skipped
            unread.push(format!("{language}: no parser pack was found"));
            continue;
        };
        match runtime.load(pack.clone()) {
            Ok(loaded) => {
                parsers.insert(language, loaded);
            }
            Err(error) => unread.push(format!("{language}: {error}")),
        }
    }
    let (mut screening, mut parsing) = (Duration::ZERO, Duration::ZERO);
    let (mut normalizing, mut reducing) = (Duration::ZERO, Duration::ZERO);
    let mut screened = 0u32;
    let mut bundled = 0u32;
    let started = Instant::now();
    // `var_os` because an unset variable is an absence rather than a failure,
    // and a variable that is set but unreadable should be said out loud.
    let budget = std::env::var_os("REPORT_BUDGET_SECONDS").and_then(|value| {
        match value.to_string_lossy().parse::<u64>() {
            Ok(seconds) => Some(Duration::from_secs(seconds)),
            Err(error) => {
                eprintln!("REPORT_BUDGET_SECONDS is not a number of seconds: {error}");
                None
            }
        }
    });
    // A workspace links its own packages into node_modules, so a library walk
    // that follows those links derives the project's own code and then reports
    // the project for reimplementing itself. Resolving both sides and dropping
    // anything that lands back inside the project is what keeps a dependency a
    // dependency.
    let project_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let mut linked_away = 0usize;
    for root in std::env::var("REPORT_LIBRARIES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|root| !root.is_empty())
    {
        let root = PathBuf::from(root);
        let mut sources = Vec::new();
        collect_library(&root, &mut sources, 0, &mut unread);
        sources.sort();
        eprintln!("{}: {} source files", root.display(), sources.len());

        // Which dependency a file belongs to, for reporting progress against
        // something a reader recognizes.
        let package_of = |path: &Path| -> String {
            let relative = path.strip_prefix(&root).unwrap_or(path);
            let mut parts = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy());
            match parts.next() {
                Some(first) if first.starts_with('@') => {
                    format!("{first}/{}", parts.next().unwrap_or_default())
                }
                Some(first) => first.into_owned(),
                None => "?".to_owned(),
            }
        };

        let mut package = String::new();
        let mut package_started = Instant::now();
        let mut package_files = 0u32;
        let mut package_behaviors = 0usize;
        let mut done = 0u32;
        for path in &sources {
            if std::fs::canonicalize(path).is_ok_and(|resolved| resolved.starts_with(&project_root))
            {
                linked_away += 1;
                continue;
            }
            if let Some(budget) = budget
                && started.elapsed() > budget
            {
                eprintln!(
                    "  budget of {budget:?} spent after {done}/{} files; stopping here",
                    sources.len()
                );
                break;
            }
            let here = package_of(path);
            if here != package {
                if !package.is_empty() {
                    eprintln!(
                        "  {package:<34} {package_files:>5} files {package_behaviors:>5} behaviors  {:>7.2?}",
                        package_started.elapsed()
                    );
                }
                package = here;
                package_started = Instant::now();
                package_files = 0;
                package_behaviors = 0;
            }
            package_files += 1;
            done += 1;
            if done % 2000 == 0 {
                let rate = f64::from(done) / started.elapsed().as_secs_f64();
                eprintln!(
                    "  … {done}/{} files, {:.0}/s, {} behaviors so far",
                    sources.len(),
                    rate,
                    library.len()
                );
            }
            let screen = Instant::now();
            let readable = is_readable_source(path, &mut unread);
            screening += screen.elapsed();
            screened += 1;
            if !readable {
                bundled += 1;
                continue;
            }
            let source = match std::fs::read(path) {
                Ok(source) => source,
                Err(error) => {
                    unread.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            let language = match path.extension().and_then(|x| x.to_str()) {
                Some("ts") => "typescript",
                _ => "javascript",
            };
            let Some(loaded) = parsers.get(language) else {
                unread.push(format!("{}: no {language} parser pack", path.display()));
                continue;
            };
            let parse_started = Instant::now();
            let parsed = loaded.parse(path.clone(), source);
            parsing += parse_started.elapsed();
            let file = match parsed {
                Ok(file) => file,
                Err(error) => {
                    unread.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            let origin = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let normalize_started = Instant::now();
            let functions = normalize_file(&file);
            normalizing += normalize_started.elapsed();
            for function in functions {
                if function.damaged {
                    continue;
                }
                let reduce_started = Instant::now();
                let form = function.form.simplify().canonical();
                let reportable = form.is_reportable();
                reducing += reduce_started.elapsed();
                if !reportable {
                    continue;
                }
                let mut names = BTreeSet::new();
                anchors(&form, &mut names);
                library.push((origin.clone(), function.name, form, names));
                package_behaviors += 1;
            }
        }
        if !package.is_empty() {
            eprintln!(
                "  {package:<34} {package_files:>5} files {package_behaviors:>5} behaviors  {:>7.2?}",
                package_started.elapsed()
            );
        }
    }
    if screened > 0 {
        let wall = started.elapsed();
        eprintln!(
            "\nlibrary: {screened} screened ({bundled} bundled, {linked_away} linked back into \
             the project), {} behaviors, {wall:.2?}",
            library.len()
        );
        for (label, spent) in [
            ("screening", screening),
            ("parsing", parsing),
            ("normalizing", normalizing),
            ("simplify+gate", reducing),
        ] {
            eprintln!(
                "  {label:<14} {spent:>9.2?}  {:>5.1}%",
                100.0 * spent.as_secs_f64() / wall.as_secs_f64()
            );
        }
    }
    library.sort_by(|left, right| left.2.to_string().cmp(&right.2.to_string()));
    library.dedup_by(|left, right| left.2 == right.2 && left.1 == right.1);

    let mut opportunities: BTreeMap<String, (String, String, BTreeSet<(String, u32, String)>)> =
        BTreeMap::new();
    for callable in &callables {
        let canonical = callable.form.canonical();
        let mut subject = BTreeSet::new();
        anchors(&canonical, &mut subject);
        for (origin, name, pattern, wanted) in &library {
            if !wanted.iter().all(|anchor| subject.contains(anchor)) {
                continue;
            }
            if canonical.matches(pattern) || canonical.contains(pattern) {
                opportunities
                    .entry(format!("{origin}::{name}"))
                    .or_insert_with(|| (origin.clone(), name.clone(), BTreeSet::new()))
                    .2
                    .extend(
                        callable
                            .lines_of(pattern)
                            .into_iter()
                            .map(|line| (callable.file.clone(), line, callable.name.clone())),
                    );
            }
        }
    }
    let mut opportunities = opportunities.into_values().collect::<Vec<_>>();
    opportunities.sort_by_key(|(origin, name, sites)| {
        (std::cmp::Reverse(sites.len()), origin.clone(), name.clone())
    });
    eprintln!("{} library behaviors derived", library.len());

    let abstractions = ranked(ABSTRACTION_MIN_NODES, ABSTRACTION_MIN_FILES, true);
    let idioms = ranked(IDIOM_MIN_NODES, IDIOM_MIN_FILES, false);

    // --- the report
    let mut md = String::new();
    let title = repository
        .slug
        .clone()
        .unwrap_or_else(|| root.display().to_string());
    let _ = writeln!(md, "# Reuse report — {title}");
    if let Some(commit) = &repository.commit {
        let _ = writeln!(
            md,
            "\n`{}`{}",
            &commit[..commit.len().min(12)],
            if repository.dirty {
                " — **working tree was dirty when this ran, so links may not match what was scanned**"
            } else {
                ""
            }
        );
    }

    let duplicate_sites: usize = duplication.iter().map(|(_, g)| g.len()).sum();
    let _ = writeln!(
        md,
        "\n**{} functions across {} files. {} behaviors implemented more than once, in {duplicate_sites} places. {} recurring shapes with no shared implementation.**",
        callables.len(),
        files.len(),
        duplication.len(),
        abstractions.len(),
    );

    let _ = writeln!(md, "\n| | |\n|---|---:|");
    let _ = writeln!(md, "| Files read | {} |", files.len());
    let _ = writeln!(md, "| Functions normalized | {} |", callables.len());
    let _ = writeln!(md, "| Skipped, unreadable syntax | {damaged} |");
    let _ = writeln!(md, "| Duplicated behaviors | {} |", duplication.len());
    let _ = writeln!(md, "| Candidate abstractions | {} |", abstractions.len());
    let _ = writeln!(md, "| Idioms observed | {} |", idioms.len());
    let _ = writeln!(
        md,
        "| Library behaviors compared against | {} |",
        library.len()
    );
    if linked_away > 0 {
        let _ = writeln!(
            md,
            "| Library files skipped as the project itself | {linked_away} |"
        );
    }
    let _ = writeln!(
        md,
        "| Reimplemented from a library | {} |",
        opportunities.len()
    );
    if !unread.is_empty() {
        let _ = writeln!(md, "| **Could not be read** | **{}** |", unread.len());
    }
    if repository.slug.is_none() || repository.commit.is_none() {
        let _ = writeln!(
            md,
            "\nNo GitHub remote or commit was found, so paths below are plain text \
             rather than links."
        );
    }
    if !unread.is_empty() {
        let _ = writeln!(
            md,
            "\n<details><summary>{} paths could not be read, and are absent from \
             everything below</summary>\n",
            unread.len()
        );
        for path in unread.iter().take(40) {
            let _ = writeln!(md, "- `{path}`");
        }
        if unread.len() > 40 {
            let _ = writeln!(md, "- … and {} more", unread.len() - 40);
        }
        let _ = writeln!(md, "</details>");
    }
    if !mirrors.is_empty() {
        let _ = writeln!(
            md,
            "| Suppressed as mirrored trees | {suppressed_duplication} |"
        );
        let _ = writeln!(
            md,
            "\nTreated as deliberate copies of one another, so a finding living \
             entirely inside them is not reported: {}. A finding that *also* appears \
             outside them still is.",
            mirrors
                .iter()
                .map(|prefix| format!("`{prefix}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let _ = writeln!(
        md,
        "\n## Duplicated behavior\n\nThe same behavior implemented in more than one file. \
         Names are not compared — these are matched on what the code *does*, so a pair that \
         was renamed on the way is still found.\n\nMost-implemented first."
    );
    if duplication.is_empty() {
        let _ = writeln!(md, "\nNothing found.");
    }
    for (form, group) in duplication.iter().take(20) {
        let names = group
            .iter()
            .map(|c| c.name.as_str())
            .collect::<BTreeSet<_>>();
        let renamed = if names.len() > 1 {
            "  ← **different names**"
        } else {
            ""
        };
        let _ = writeln!(
            md,
            "\n### `{}` — {} implementations{renamed}\n",
            names.iter().copied().collect::<Vec<_>>().join("` / `"),
            group.len()
        );
        for callable in group {
            let _ = writeln!(
                md,
                "- {} — `{}`",
                repository.link(&callable.file, callable.line),
                callable.name
            );
        }
        let _ = writeln!(
            md,
            "\n<details><summary>shared form</summary>\n\n```\n{form}\n```\n</details>"
        );
    }

    let _ = writeln!(
        md,
        "\n## Already provided by a library\n\nCode that reimplements something a dependency \
         already does. The library's behavior is derived from its own source, so nothing here \
         is a hand-written list of APIs — and the two are matched on behavior, so a renamed \
         reimplementation is still found.\n\nMost sites first."
    );
    if opportunities.is_empty() {
        let _ = writeln!(
            md,
            "\nNothing found{}.",
            if library.is_empty() {
                " — no library was given, so this was not looked at"
            } else {
                ""
            }
        );
    }
    for (origin, name, sites) in opportunities.iter().take(20) {
        let _ = writeln!(
            md,
            "\n### `{name}` — {} site{}\n\nProvided by `{origin}`.\n",
            sites.len(),
            if sites.len() == 1 { "" } else { "s" }
        );
        for (file, line, function) in sites.iter().take(8) {
            let _ = writeln!(md, "- {} — `{function}`", repository.link(file, *line));
        }
        if sites.len() > 8 {
            let _ = writeln!(md, "- … and {} more", sites.len() - 8);
        }
    }

    let _ = writeln!(
        md,
        "\n## Candidate abstractions\n\nA shape with real structure, repeated across files that \
         do not otherwise share code. No single occurrence is worth extracting; the count is \
         the argument.\n\nMost sites first."
    );
    if abstractions.is_empty() {
        let _ = writeln!(md, "\nNothing found.");
    }
    for (form, shape) in abstractions.iter().take(15) {
        let _ = writeln!(
            md,
            "\n### {} files, {} sites\n",
            shape.files.len(),
            shape.sites.len()
        );
        let _ = writeln!(md, "```\n{form}\n```\n");
        for (file, line, name) in shape.sites.iter().take(8) {
            let _ = writeln!(md, "- {} — `{name}`", repository.link(file, *line));
        }
        if shape.sites.len() > 8 {
            let _ = writeln!(md, "- … and {} more", shape.sites.len() - 8);
        }
    }

    let _ = writeln!(
        md,
        "\n## Idioms\n\nSmall shapes repeated widely. **These are not defects.** They are how \
         the language is written here, and they are listed so that the section above can be \
         read as distinct from them. A few may still be worth a helper — judgement, not a rule.\n\n\
         Most sites first."
    );
    let _ = writeln!(md, "\n| files | sites | shape |\n|---:|---:|---|");
    for (form, shape) in idioms.iter().take(15) {
        let _ = writeln!(
            md,
            "| {} | {} | `{form}` |",
            shape.files.len(),
            shape.sites.len()
        );
    }

    let _ = writeln!(
        md,
        "\n## How this was produced\n\nEvery function is normalized into a language-neutral \
         behavior form: names become positions, values the code does not branch on become \
         holes, and every spelling of iteration becomes one construct. Two functions that do \
         the same thing reduce to the same form even when nothing about their text agrees.\n\n\
         Duplication compares whole functions. Shapes compare every subtree, so a pattern is \
         found wherever it sits inside a larger function.\n\n\
         ### What this cannot tell you\n\n\
         - Whether duplication is **intentional**. A deliberate fork and an accident look \
           identical here.\n\
         - Whether two copies have **diverged**. They matched because they agree; if one has \
           since been fixed and the other not, that is a bug this will not show.\n\
         - Whether a shape *should* be extracted. Frequency is evidence, not a verdict.\n\n\
         ### Thresholds\n\n\
         Abstractions: at least {ABSTRACTION_MIN_NODES} nodes, in at least \
         {ABSTRACTION_MIN_FILES} files. Idioms: at least {IDIOM_MIN_NODES} nodes, in at least \
         {IDIOM_MIN_FILES} files. Nothing above {LARGEST_SHAPE} nodes is treated as a shape. \
         These are calibrated against one repository and should be treated as a starting \
         point, not a measurement."
    );

    std::fs::write(&out, md).expect("write the report");
    eprintln!(
        "{} files, {} functions -> {}",
        files.len(),
        callables.len(),
        out.display()
    );
}
