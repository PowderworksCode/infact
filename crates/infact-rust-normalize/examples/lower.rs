//! Lowering a `Form` back to Rust, as well as the form permits.
//!
//! This is deliberately the most generous emitter the form supports. Where a
//! variant discarded its source it guesses the most common spelling — a
//! `Construct` becomes `T::new()`, a peeled adapter is not reinstated — so that
//! what fails to compile fails for want of information rather than for want of
//! effort. Every guess is counted, which is the measurement: a guess is a place
//! the form did not keep enough to emit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use infact_normalize::{Coverage, Direction, Form, Pattern};
use infact_rust_normalize::normalize_file;

/// A place the form did not keep enough to emit, by what was missing.
#[derive(Debug, Default)]
struct Guesses {
    counts: RefCell<BTreeMap<&'static str, u64>>,
}

impl Guesses {
    fn note(&self, what: &'static str) {
        *self.counts.borrow_mut().entry(what).or_default() += 1;
    }

    fn total(&self) -> u64 {
        self.counts.borrow().values().sum()
    }
}

/// What each role was called in the source, when a caller supplies it.
///
/// This is the side-table shape: the form is unchanged and the names sit
/// beside it, so matching is untouched and lowering can still say `counts`
/// where the form says `v0`.
#[derive(Default)]
struct Names {
    locals: BTreeMap<u32, String>,
    frees: BTreeMap<u32, String>,
}

impl Names {
    fn from(ledger: &[(Form, String)]) -> Self {
        let mut names = Self::default();
        for (role, name) in ledger {
            match role {
                Form::Local(index) => {
                    names.locals.entry(*index).or_insert_with(|| name.clone());
                }
                Form::Free(index) => {
                    names.frees.entry(*index).or_insert_with(|| name.clone());
                }
                _ => {}
            }
        }
        names
    }

    fn local(&self, index: u32) -> String {
        self.locals
            .get(&index)
            .cloned()
            .unwrap_or_else(|| format!("v{index}"))
    }

    fn free(&self, index: u32) -> String {
        self.frees
            .get(&index)
            .cloned()
            .unwrap_or_else(|| format!("f{index}"))
    }
}

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

fn pattern(pattern: &Pattern, names: &Names) -> String {
    match pattern {
        // The identifier is gone; binding order is all that survived.
        Pattern::Binding(index) => names.local(*index),
        Pattern::Ignored => "_".to_owned(),
        Pattern::Tuple(parts) => format!(
            "({})",
            parts
                .iter()
                .map(|part| self::pattern(part, names))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Pattern::Variant { name, parts } if parts.is_empty() => name.clone(),
        Pattern::Variant { name, parts } => format!(
            "{name}({})",
            parts
                .iter()
                .map(|part| self::pattern(part, names))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn lower(form: &Form, level: usize, guesses: &Guesses, names: &Names) -> String {
    let sub = |form: &Form| lower(form, level, guesses, names);
    let list = |forms: &[Form]| forms.iter().map(&sub).collect::<Vec<_>>().join(", ");
    match form {
        Form::Local(index) => names.local(*index),
        Form::Free(index) => names.free(*index),
        Form::Literal => "()".to_owned(),
        Form::Constant(value) => value.clone(),
        Form::Number(value) => value.clone(),
        // The constructing function and every argument it took were discarded.
        // `new` is the commonest spelling, and a guess is all this can be.
        Form::Construct(name) => {
            guesses.note("Construct: constructor and arguments discarded");
            format!("{name}::new()")
        }
        Form::Variant { name, payload } if payload.is_empty() => name.clone(),
        Form::Variant { name, payload } => format!("{name}({})", list(payload)),
        Form::Path(path) => path.clone(),
        Form::Field { value, name } => format!("{}.{name}", sub(value)),
        Form::Method {
            name,
            receiver,
            arguments,
        } => format!("{}.{name}({})", sub(receiver), list(arguments)),
        Form::Call { callee, arguments } => format!("{}({})", sub(callee), list(arguments)),
        Form::Traverse {
            sequence,
            item,
            body,
            direction,
        } => {
            if matches!(direction, Direction::Backward) {
                guesses.note("Traverse: backward walk has no recorded spelling");
            }
            format!(
                "for {} in {} {{\n{}{}\n{}}}",
                pattern(item, names),
                sub(sequence),
                indent(level + 1),
                lower(body, level + 1, guesses, names),
                indent(level)
            )
        }
        // The pairs are what the form records; which loop bounds produced them
        // is not, so this is emitted as the library call that offers them.
        Form::Pairwise {
            sequence,
            left,
            right,
            body,
            coverage,
        } => {
            guesses.note("Pairwise: the loop bounds that produced the pairs are not recorded");
            let call = match coverage {
                Coverage::Once => "tuple_combinations()",
                // Each pair both ways round is what `permutations(2)` yields.
                Coverage::BothWays => "permutations(2)",
            };
            format!(
                "for ({}, {}) in {}.{call} {{\n{}{}\n{}}}",
                pattern(left, names),
                pattern(right, names),
                sub(sequence),
                indent(level + 1),
                lower(body, level + 1, guesses, names),
                indent(level)
            )
        }
        // Every adapter that fed the sequence was peeled off as noise, so the
        // receiver is emitted bare and will not be an iterator.
        Form::Transform {
            sequence,
            item,
            body,
        } => {
            guesses.note("Transform: iterator adapters peeled off the sequence");
            format!(
                "{}.map(|{}| {})",
                sub(sequence),
                pattern(item, names),
                lower(body, level, guesses, names)
            )
        }
        Form::Sift {
            sequence,
            item,
            body,
        } => {
            guesses.note("Sift: iterator adapters peeled off the sequence");
            format!(
                "{}.filter_map(|{}| {})",
                sub(sequence),
                pattern(item, names),
                lower(body, level, guesses, names)
            )
        }
        // `filter`, `take_while` and `skip_while` are one variant, so which was
        // written is gone.
        Form::Retain {
            sequence,
            item,
            body,
        } => {
            guesses.note("Retain: filter/take_while/skip_while collapsed to one variant");
            format!(
                "{}.filter(|{}| {})",
                sub(sequence),
                pattern(item, names),
                lower(body, level, guesses, names)
            )
        }
        Form::Accumulate {
            sequence,
            initial,
            accumulator,
            item,
            body,
        } => {
            guesses.note("Accumulate: fold/try_fold collapsed to one variant");
            format!(
                "{}.fold({}, |{}, {}| {})",
                sub(sequence),
                sub(initial),
                pattern(accumulator, names),
                pattern(item, names),
                lower(body, level, guesses, names)
            )
        }
        Form::Collect {
            sequence,
            container,
        } => match container {
            Some(container) => format!("{}.collect::<{container}<_>>()", sub(sequence)),
            None => {
                guesses.note("Collect: container type was inferred and not recorded");
                format!("{}.collect()", sub(sequence))
            }
        },
        Form::Assign {
            operator,
            target,
            value,
        } => format!("{} {operator} {}", sub(target), sub(value)),
        Form::Binary {
            operator,
            left,
            right,
        } => format!("({} {operator} {})", sub(left), sub(right)),
        Form::Unary { operator, value } => format!("{operator}{}", sub(value)),
        Form::Index { sequence, position } => format!("{}[{}]", sub(sequence), sub(position)),
        Form::Span {
            start,
            end,
            inclusive,
        } => {
            let operator = if *inclusive { "..=" } else { ".." };
            format!("{}{operator}{}", sub(start), sub(end))
        }
        Form::Lambda { parameters, body } => format!(
            "|{}| {}",
            parameters
                .iter()
                .map(|parameter| self::pattern(parameter, names))
                .collect::<Vec<_>>()
                .join(", "),
            lower(body, level, guesses, names)
        ),
        // Whether the binding was `mut` is not in the form, and a `let` that
        // should be mutable will not compile if it is emitted plain.
        Form::Let {
            pattern: bound,
            value,
        } => {
            guesses.note("Let: mutability and type annotation discarded");
            format!("let {} = {};", pattern(bound, names), sub(value))
        }
        Form::Branch {
            condition,
            consequence,
            alternative,
        } => {
            let taken = format!(
                "if {} {{\n{}{}\n{}}}",
                sub(condition),
                indent(level + 1),
                lower(consequence, level + 1, guesses, names),
                indent(level)
            );
            match alternative {
                Some(alternative) => format!(
                    "{taken} else {{\n{}{}\n{}}}",
                    indent(level + 1),
                    lower(alternative, level + 1, guesses, names),
                    indent(level)
                ),
                None => taken,
            }
        }
        // Arms are held sorted by what they name, so the order they were
        // written in is gone. That is only safe when no arm has a guard, and a
        // guarded `match` never reaches here — it is `Opaque`.
        Form::Select { scrutinee, arms } => {
            guesses.note("Select: arms reordered canonically");
            let arms = arms
                .iter()
                .map(|arm| {
                    format!(
                        "{}{} => {},",
                        indent(level + 1),
                        pattern(&arm.pattern, names),
                        lower(&arm.body, level + 1, guesses, names)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("match {} {{\n{arms}\n{}}}", sub(scrutinee), indent(level))
        }
        Form::Return(value) => format!("return {}", sub(value)),
        Form::Sequence(steps) => {
            let body = steps
                .iter()
                .enumerate()
                .map(|(position, step)| {
                    let text = lower(step, level, guesses, names);
                    let last = position + 1 == steps.len();
                    // A statement needs its semicolon; the tail expression is
                    // the value. Which steps were statements is not recorded,
                    // so this is inferred from position.
                    if last || text.ends_with(';') || text.ends_with('}') {
                        format!("{}{text}", indent(level))
                    } else {
                        format!("{}{text};", indent(level))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{{\n{body}\n{}}}", indent(level.saturating_sub(1)))
        }
        // Syntax the lift had no shape for. A few kinds can be put back
        // because the spelling follows from the kind; the rest cannot.
        Form::Opaque { kind, parts } => match kind.as_str() {
            "try_expression" if parts.len() == 1 => format!("{}?", sub(&parts[0])),
            "tuple_expression" => format!("({})", list(parts)),
            "array_expression" => format!("[{}]", list(parts)),
            "index_expression" if parts.len() == 2 => {
                format!("{}[{}]", sub(&parts[0]), sub(&parts[1]))
            }
            "continue_expression" => "continue".to_owned(),
            "break_expression" if parts.is_empty() => "break".to_owned(),
            "reference_expression" if parts.len() == 1 => format!("&{}", sub(&parts[0])),
            _ => {
                guesses.note("Opaque: syntax the lift kept no shape for");
                format!("/* {kind} */ todo!()")
            }
        },
    }
}

/// Whether a `Select` arm's body names a local the arm did not bind.
///
/// `Roles` numbers bindings across a whole function with no notion of scope, so
/// a name bound by one arm stays resolvable in the next. Matching never had to
/// care. Lowering does: emitting the arm reproduces the reference, and the
/// result is not a compile error but a different program.
fn leaks_scope(form: &Form) -> bool {
    if let Form::Select { arms, .. } = form {
        for (position, arm) in arms.iter().enumerate() {
            for earlier in &arms[..position] {
                let mut bound = Vec::new();
                collect_bindings(&earlier.pattern, &mut bound);
                for index in bound {
                    if !arm.pattern.binds(index) && arm.body.references_local(index) {
                        return true;
                    }
                }
            }
        }
    }
    form.children().into_iter().any(leaks_scope)
}

fn collect_bindings(pattern: &Pattern, found: &mut Vec<u32>) {
    match pattern {
        Pattern::Binding(index) => found.push(*index),
        Pattern::Ignored => {}
        Pattern::Tuple(parts) | Pattern::Variant { parts, .. } => {
            for part in parts {
                collect_bindings(part, found);
            }
        }
    }
}

fn rust_files(root: &std::path::Path, found: &mut Vec<PathBuf>) {
    if root.is_file() {
        found.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Rewrite every function body in a file with what lowering produced.
///
/// The signature, the imports and the types around it are left exactly as
/// written: only what was lifted is replaced by what came back. Anything that
/// then fails to compile failed because the form did not keep it.
fn splice(source: &[u8], functions: &[(u64, u64, String)]) -> String {
    let mut output = String::new();
    let mut cursor = 0usize;
    let mut ordered = functions.to_vec();
    ordered.sort_by_key(|(start, _, _)| *start);
    // A body inside another function's body would be replaced twice.
    let mut written_to = 0u64;
    for (start, end, body) in ordered {
        if start < written_to {
            continue;
        }
        output.push_str(&String::from_utf8_lossy(&source[cursor..start as usize]));
        output.push_str(&body);
        cursor = end as usize;
        written_to = end;
    }
    output.push_str(&String::from_utf8_lossy(&source[cursor..]));
    output
}

/// Write each file back with every body replaced by its lowering.
///
/// One function at a time is the honest measurement, but a whole file at once
/// answers the prior question — whether anything survives at all — for the
/// price of a single compile.
fn splice_main(roots: &[PathBuf], destination: &std::path::Path) {
    let pack = Arc::new(
        ParserPack::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust"),
        )
        .expect("rust parser pack"),
    );
    let parser = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser");

    let mut files = Vec::new();
    for root in roots {
        rust_files(root, &mut files);
    }
    let guesses = Guesses::default();
    let single = std::env::var("LOWER_ONLY").ok();
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(parsed) = parser.parse(
            path.to_string_lossy().as_ref(),
            Arc::<[u8]>::from(bytes.as_slice()),
        ) else {
            continue;
        };
        let bodies = normalize_file(&parsed)
            .into_iter()
            .filter(|function| single.as_ref().is_none_or(|only| *only == function.name))
            .map(|function| {
                (
                    function.body_start_byte,
                    function.body_end_byte,
                    lower(&function.form, 1, &guesses, &Names::from(&function.names)),
                )
            })
            .collect::<Vec<_>>();
        let target = destination.join(path.file_name().unwrap_or_default());
        std::fs::write(&target, splice(&bytes, &bodies)).expect("writing spliced source");
        eprintln!("spliced {} bodies into {}", bodies.len(), target.display());
    }
}

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let show = arguments.iter().any(|argument| argument == "--show");
    // `--splice-into <dir>`: write each source file back with lowered bodies.
    if let Some(position) = arguments
        .iter()
        .position(|argument| argument == "--splice-into")
    {
        let destination = PathBuf::from(&arguments[position + 1]);
        let roots = arguments[..position]
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        splice_main(&roots, &destination);
        return;
    }
    let roots = arguments
        .iter()
        .filter(|argument| !argument.starts_with("--"))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        eprintln!("usage: lower [--show] <path>...");
        std::process::exit(2);
    }

    let pack = Arc::new(
        ParserPack::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust"),
        )
        .expect("rust parser pack"),
    );
    let parser = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser");

    let mut files = Vec::new();
    for root in &roots {
        rust_files(root, &mut files);
    }
    files.sort();

    let guesses = Guesses::default();
    let mut total = 0u64;
    let mut clean = 0u64;
    let mut with_todo = 0u64;
    let mut leaking = 0u64;
    let mut eligible = 0u64;

    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(parsed) = parser.parse(
            path.to_string_lossy().as_ref(),
            Arc::<[u8]>::from(bytes.as_slice()),
        ) else {
            continue;
        };
        for function in normalize_file(&parsed) {
            let before = guesses.total();
            let emitted = lower(&function.form, 1, &guesses, &Names::from(&function.names));
            let made = guesses.total() - before;
            total += 1;
            if made == 0 {
                clean += 1;
            }
            if emitted.contains("todo!()") {
                with_todo += 1;
            }
            // What actually predicted a round trip, measured: nothing
            // constructed, no macro, no decision, no syntax the lift had no
            // shape for. Mutability is discarded too, but a `let` that was not
            // mutable still round-trips, so it does not belong in the test.
            let form = function.form.to_string();
            if !emitted.contains("todo!()")
                && !form.contains("(construct ")
                && !form.contains("(select ")
            {
                eligible += 1;
            }
            let leaks = leaks_scope(&function.form);
            if leaks {
                leaking += 1;
            }
            if show {
                println!(
                    "// ---- {} ({}:{}) — {made} guesses{}",
                    function.name,
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    function.start_line,
                    if leaks { ", SCOPE LEAK" } else { "" }
                );
                println!("// original:");
                for line in String::from_utf8_lossy(&bytes)
                    [function.start_byte as usize..function.end_byte as usize]
                    .lines()
                {
                    println!("//   {line}");
                }
                println!("fn {}() {emitted}\n", function.name);
            }
        }
    }

    eprintln!("# lowering {total} functions");
    eprintln!(
        "emitted with no guess at all   {clean} ({:.1}%)",
        100.0 * clean as f64 / total as f64
    );
    eprintln!(
        "emitted containing todo!()     {with_todo} ({:.1}%)",
        100.0 * with_todo as f64 / total as f64
    );
    eprintln!(
        "free of construction, macros and decisions  {eligible} ({:.1}%)",
        100.0 * eligible as f64 / total as f64
    );
    eprintln!(
        "arm body names another arm's binding  {leaking} ({:.1}%)",
        100.0 * leaking as f64 / total as f64
    );
    eprintln!();
    eprintln!("# guesses by cause ({} total)", guesses.total());
    let mut ordered = guesses
        .counts
        .borrow()
        .iter()
        .map(|(cause, count)| (*count, *cause))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(count, cause)| (std::cmp::Reverse(*count), *cause));
    for (count, cause) in ordered {
        eprintln!("{count:>6}  {cause}");
    }
}
