//! Discarded-error facts derived from source, in any language whose parser
//! pack ships the queries.
//!
//! A fallible expression whose error is dropped cannot reach a caller, so a
//! failure at that site produces a quieter run rather than a louder one. This
//! analyzer records where that happens and how much the enclosing callable
//! could have done about it. It decides no policy: whether a given form is
//! permitted belongs to the consumer.
//!
//! Syntax is the floor. `.ok()` and an `Err(_)` arm name `Result` and nothing
//! else, so those are reported as certain. `.unwrap_or_default()` reads the
//! same on `Option`, and no type is resolved here, so those are reported as
//! possible and left for a consumer to weigh.
//!
//! Nothing here names a syntax node kind. Recognition lives in the pack's
//! `discards.scm` and `callables.scm`; which type names mean failure, what
//! marks a test, and which forms are ambiguous live in its `parser.toml`. A
//! new language is a pack, not a code change.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{
    ErrorHandlingManifest, LoadedParser, ParsedFile, ParserCatalog, Propagation, parse_repository,
};
use infact_core::{
    CallEdgeEvidence, Certainty, Containment, Derivation, DiscardForm, ErrorDiscard, Fact,
    InputEvidence, Reach, SourceSpan,
};
use pathfinding::directed::dijkstra::{build_path, dijkstra_all};
use tree_sitter::Node;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("loading parser pack: {0}")]
    Parser(#[from] entl_tree_sitter::Error),
    #[error("source file {} is too large for source coordinates", path.display())]
    SourceTooLarge { path: PathBuf },
    #[error("no loaded parser for pack {pack}, whose queries the analysis needs")]
    MissingParser { pack: String },
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDiagnostic {
    pub path: PathBuf,
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositoryErrorReport {
    pub discards: Vec<Fact<ErrorDiscard>>,
    pub diagnostics: Vec<ErrorDiagnostic>,
}

/// Find every discarded-error site in a repository.
///
/// A file is analyzed when its pack ships the queries this needs. A pack that
/// ships neither describes no discard forms for its language, which is a
/// different thing from a language having none, so it is skipped in silence
/// rather than reported as clean.
pub fn analyze_repository_errors(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
) -> Result<RepositoryErrorReport> {
    let parsed = parse_repository(root, parsers)?;
    let mut discards = Vec::new();
    let mut callables = Vec::new();
    let mut inputs = BTreeMap::new();
    for file in &parsed.files {
        let Some(parser) = parsed.parsers.get(&file.pack.manifest().id) else {
            continue;
        };
        if !describes_discards(parser) {
            continue;
        }
        inputs.insert(file.path.clone(), input_evidence(file));
        let facts = collect_file(parser, file)?;
        discards.extend(facts.discards);
        callables.extend(facts.callables);
    }
    // The call graph spans files, so reach is resolved once over the whole
    // repository rather than per file.
    resolve_reach(&mut discards, &callables);
    discards.sort();
    discards.dedup();

    let facts = discards
        .into_iter()
        .map(|value| Fact {
            derivation: discard_derivation(&value, &inputs),
            value,
        })
        .collect();

    let mut diagnostics = parsed
        .diagnostics
        .into_iter()
        .map(|diagnostic| ErrorDiagnostic {
            path: diagnostic.path,
            line: 1,
            message: diagnostic.message,
        })
        .collect::<Vec<_>>();
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.line.cmp(&right.line))
            .then(left.message.cmp(&right.message))
    });
    diagnostics.dedup();

    Ok(RepositoryErrorReport {
        discards: facts,
        diagnostics,
    })
}

/// Find the discarded-error sites in a single parsed file.
///
/// Reach is resolved against this file alone, so a caller in another file is
/// not seen. Repository analysis resolves it across every file.
pub fn analyze_file(parser: &LoadedParser, file: &ParsedFile) -> Result<Vec<ErrorDiscard>> {
    let mut facts = collect_file(parser, file)?;
    resolve_reach(&mut facts.discards, &facts.callables);
    facts.discards.sort();
    facts.discards.dedup();
    Ok(facts.discards)
}

/// The queries a pack must ship to describe its language's discard forms.
const REQUIRED_QUERIES: [&str; 2] = ["callables", "discards"];

/// Whether a pack says anything about discarded errors at all.
fn describes_discards(parser: &LoadedParser) -> bool {
    let available = parser.query_names().collect::<BTreeSet<_>>();
    REQUIRED_QUERIES.iter().all(|name| available.contains(name))
}

/// What one file contributes: its discards and its slice of the call graph.
struct FileFacts {
    discards: Vec<ErrorDiscard>,
    callables: Vec<CallableNode>,
}

/// A callable, its signature's verdict, and the calls written inside it.
struct CallableNode {
    path: String,
    name: String,
    containment: Containment,
    calls: Vec<Callsite>,
}

struct Callsite {
    callee: String,
    span: SourceSpan,
}

/// The structural scaffolding a file provides, all of it from `callables.scm`.
///
/// Nothing here names a syntax node kind. A callable is whatever the pack's
/// query captured as one, and which callable holds a discard is decided by
/// byte-range containment, which is a property of trees rather than of any
/// language.
struct Scaffold<'tree> {
    /// Callables in document order, so an index is stable across runs.
    callables: Vec<ScaffoldCallable<'tree>>,
    /// Implementation blocks, for the `{module}::{implementation}::{name}` path.
    implementations: Vec<(String, Region)>,
    /// Regions the pack's test markers cover.
    tests: Vec<Region>,
    /// Call sites, as a callee name and the node to take a span from.
    calls: Vec<(String, Node<'tree>)>,
}

struct ScaffoldCallable<'tree> {
    name: String,
    item: Region,
    body: Region,
    containment: Containment,
    node: Node<'tree>,
}

/// A half-open byte range.
type Region = (usize, usize);

fn region(node: Node<'_>) -> Region {
    (node.start_byte(), node.end_byte())
}

fn covers(outer: Region, point: usize) -> bool {
    outer.0 <= point && point < outer.1
}

/// The innermost region covering a point, or none if nothing does.
fn innermost<T>(items: &[T], point: usize, region_of: impl Fn(&T) -> Region) -> Option<&T> {
    items
        .iter()
        .filter(|item| covers(region_of(item), point))
        .min_by_key(|item| {
            let (start, end) = region_of(item);
            end - start
        })
}

fn scaffold<'tree>(
    parser: &'tree LoadedParser,
    file: &'tree ParsedFile,
) -> Result<Scaffold<'tree>> {
    let matches = parser.matches("callables", file)?;
    let source = &file.source;
    let declared = &file.pack.manifest().error_handling;
    let markers = &file.pack.manifest().tests;

    // A return type is matched by its own pattern, so it is collected first and
    // joined to the callable by the body they share.
    let mut returns: BTreeMap<Region, &str> = BTreeMap::new();
    for matched in &matches {
        if let (Some(item), Some(declared)) = (
            matched.capture("callable.with-return"),
            matched.capture("callable.return"),
        ) && let Some(text) = node_text(declared, source)
        {
            returns.insert(region(item), text);
        }
    }

    let mut callables = Vec::new();
    for matched in &matches {
        let (Some(item), Some(name), Some(body)) = (
            matched.capture("callable.item"),
            matched.capture("callable.name"),
            matched.capture("callable.body"),
        ) else {
            continue;
        };
        let Some(name) = node_text(name, source) else {
            continue;
        };
        callables.push(ScaffoldCallable {
            name: name.to_owned(),
            item: region(item),
            body: region(body),
            containment: containment_of(returns.get(&region(item)).copied(), declared),
            node: item,
        });
    }
    // Document order, because `resolve_reach` breaks ties on the index and a
    // query's match order is not a promise.
    callables.sort_by_key(|callable| callable.item);
    callables.dedup_by_key(|callable| callable.item);

    let mut implementations = Vec::new();
    let mut tests = Vec::new();
    let mut calls = Vec::new();
    for matched in &matches {
        if let (Some(item), Some(declared)) =
            (matched.capture("impl.item"), matched.capture("impl.type"))
            && let Some(text) = node_text(declared, source)
        {
            implementations.push((simple_type_name(text), region(item)));
        }
        if let (Some(item), Some(body)) = (
            matched.capture("attribute.item"),
            matched.capture("attribute.text"),
        ) && let Some(text) = node_text(body, source)
        {
            let flattened = text.replace(char::is_whitespace, "");
            let marks = markers
                .markers
                .iter()
                .chain(&markers.module_markers)
                .any(|marker| flattened.contains(&marker.replace(char::is_whitespace, "")));
            if marks && let Some(region) = attributed_region(item) {
                tests.push(region);
            }
        }
        if let (Some(site), Some(callee)) =
            (matched.capture("call.site"), matched.capture("call.callee"))
            && let Some(name) = node_text(callee, source)
        {
            calls.push((name.to_owned(), site));
        }
    }
    implementations.sort_by_key(|(_, region)| *region);
    implementations.dedup();
    tests.sort_unstable();
    tests.dedup();
    calls.sort_by_key(|(_, node)| region(*node));
    calls.dedup_by_key(|(_, node)| region(*node));

    Ok(Scaffold {
        callables,
        implementations,
        tests,
        calls,
    })
}

/// What an annotation applies to.
///
/// An annotation written above an item marks the item that follows it. One
/// written inside a body marks the whole item that owns the body, which is how
/// a module marks itself rather than its first declaration.
fn attributed_region(item: Node<'_>) -> Option<Region> {
    if let Some(next) = item.next_named_sibling()
        && next.kind() != item.kind()
    {
        return Some((item.start_byte(), next.end_byte()));
    }
    item.parent().map(region)
}

/// Whether a callable can report a failure, read from what the pack declares.
fn containment_of(declared: Option<&str>, types: &ErrorHandlingManifest) -> Containment {
    // A return type the pack does not recognize means different things in
    // different languages. Where failure is declared, it means the failure has
    // nowhere to go. Where failure is unchecked, it means the signature had
    // nothing to say and the callable could still have left the failure alone;
    // calling that infallible would claim the error was trapped when not
    // catching it was available the whole time.
    let unrecognized = match types.propagation {
        Propagation::Declared => Containment::Infallible,
        Propagation::Unchecked => Containment::Fallible,
    };
    let Some(declared) = declared else {
        return unrecognized;
    };
    let leading = declared.split('<').next().unwrap_or(declared);
    if types
        .fallible_types
        .iter()
        .any(|name| leading.contains(name))
    {
        Containment::Fallible
    } else if types
        .optional_types
        .iter()
        .any(|name| leading.contains(name))
    {
        Containment::Optional
    } else {
        unrecognized
    }
}

/// Every capture that names a discard form, in the order the pack declares them.
const FORMS: &[(&str, DiscardForm)] = &[
    ("discard.let-underscore", DiscardForm::LetUnderscore),
    ("discard.ok-binding", DiscardForm::OkBinding),
    ("discard.err-arm", DiscardForm::ErrArm),
    ("discard.ok-discard", DiscardForm::OkDiscard),
    ("discard.unwrap-or", DiscardForm::UnwrapOr),
    ("discard.cause-erased", DiscardForm::CauseErased),
    ("discard.panic", DiscardForm::Panic),
    ("discard.iterator-drop", DiscardForm::IteratorDrop),
];

fn collect_file(parser: &LoadedParser, file: &ParsedFile) -> Result<FileFacts> {
    let module = repository_module(&file.path);
    let source = &file.source;
    let declared = &file.pack.manifest().error_handling;
    let scaffold = scaffold(parser, file)?;

    let path_of = |callable: &ScaffoldCallable<'_>| match innermost(
        &scaffold.implementations,
        callable.item.0,
        |(_, region)| *region,
    ) {
        Some((implementation, _)) => {
            format!("{module}::{implementation}::{}", callable.name)
        }
        None => format!("{module}::{}", callable.name),
    };

    let mut callables = scaffold
        .callables
        .iter()
        .map(|callable| CallableNode {
            path: path_of(callable),
            name: callable.name.clone(),
            containment: callable.containment,
            calls: Vec::new(),
        })
        .collect::<Vec<_>>();

    // A call belongs to the callable whose body encloses it. One written
    // outside every callable belongs to nothing, and is dropped rather than
    // attributed to the module, because a module cannot be called.
    for (callee, site) in &scaffold.calls {
        let Some(index) = scaffold
            .callables
            .iter()
            .enumerate()
            .filter(|(_, callable)| covers(callable.body, site.start_byte()))
            .min_by_key(|(_, callable)| callable.body.1 - callable.body.0)
            .map(|(index, _)| index)
        else {
            continue;
        };
        callables[index].calls.push(Callsite {
            callee: callee.clone(),
            span: source_span(&file.path, *site)?,
        });
    }

    let mut discards = Vec::new();
    for matched in parser.matches("discards", file)? {
        let Some((capture, form)) = FORMS
            .iter()
            .find(|(capture, _)| matched.has(capture))
            .map(|(capture, form)| (*capture, *form))
        else {
            continue;
        };
        // A capture the pack marks as a binding means the cause was read, so
        // nothing was discarded. Queries have no negation; absence is the claim.
        if matched.has(&format!("{capture}.bind")) {
            continue;
        }
        let Some(site) = matched.capture(capture) else {
            continue;
        };
        let expression = matched
            .capture(&format!("{capture}.expression"))
            .and_then(|node| node_text(node, source))
            .unwrap_or_default();
        // A receiver whose own call answers a query discards nothing: an `Err`
        // that means "not present" is the answer, not a failure. Only the forms
        // the pack names take this, because only they identify the discard by
        // the failure type -- `.unwrap_or(..)` says nothing about what it
        // unwrapped, so excluding it there would drop real findings.
        if declared
            .non_failure_results_forms
            .iter()
            .any(|name| name == form.as_str())
            && matched
                .capture(&format!("{capture}.expression"))
                .and_then(|node| receiver_method(node, source))
                .is_some_and(|method| {
                    declared
                        .non_failure_results
                        .iter()
                        .any(|name| name == method)
                })
        {
            continue;
        }
        let certainty = if declared
            .ambiguous_forms
            .iter()
            .any(|name| name == form.as_str())
        {
            Certainty::Possible
        } else {
            Certainty::Certain
        };

        let enclosing = innermost(&scaffold.callables, site.start_byte(), |callable| {
            callable.body
        });
        let (callable, callable_span, containment) = match enclosing {
            Some(callable) => (
                path_of(callable),
                source_span(&file.path, callable.node)?,
                callable.containment,
            ),
            // Outside every callable the failure has no signature to escape
            // through, and the file's module is the honest attribution.
            None => (
                module.clone(),
                source_span(&file.path, file.tree.root_node())?,
                Containment::Infallible,
            ),
        };
        discards.push(ErrorDiscard {
            callable,
            callable_span,
            form,
            containment,
            certainty,
            expression: truncate(expression),
            span: source_span(&file.path, site)?,
            in_test: scaffold
                .tests
                .iter()
                .any(|region| covers(*region, site.start_byte())),
            reach: Reach::Unknown,
            path: Vec::new(),
        });
    }

    Ok(FileFacts {
        discards,
        callables,
    })
}

/// The method a receiver expression itself calls, when it calls one.
fn receiver_method<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node_text(
        node.child_by_field_name("function")?
            .child_by_field_name("field")?,
        source,
    )
}

/// Answer, for each discard, how far up the failure could have travelled.
///
/// `Containment` reads one signature. This reads the callers too, because a
/// discard inside an infallible callable that only infallible callables ever
/// call cannot be reported anywhere — changing the immediate signature would
/// not be enough, and the report should say so.
///
/// Calls are resolved by name, and only when exactly one callable answers to
/// it. An ambiguous or unresolvable name yields no edge, which is why the
/// absence of callers is reported as `Unknown` rather than as `Sealed`.
fn resolve_reach(discards: &mut [ErrorDiscard], callables: &[CallableNode]) {
    let mut by_path = BTreeMap::new();
    for (index, callable) in callables.iter().enumerate() {
        by_path.insert(callable.path.as_str(), index);
    }

    // A name shared by several callables cannot be resolved to one of them.
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, callable) in callables.iter().enumerate() {
        by_name
            .entry(callable.name.as_str())
            .or_default()
            .push(index);
    }

    // callee -> the callers that reach it, with the call site
    let mut callers: BTreeMap<usize, Vec<(usize, &Callsite)>> = BTreeMap::new();
    for (caller, callable) in callables.iter().enumerate() {
        for call in &callable.calls {
            let Some(targets) = by_name.get(call.callee.as_str()) else {
                continue;
            };
            let [callee] = targets.as_slice() else {
                continue;
            };
            if *callee == caller {
                continue;
            }
            callers.entry(*callee).or_default().push((caller, call));
        }
    }

    for discard in discards {
        if discard.containment == Containment::Fallible {
            discard.reach = Reach::Local;
            continue;
        }
        let Some(&start) = by_path.get(discard.callable.as_str()) else {
            continue;
        };
        let (reach, path) = search_upward(start, &callers, callables);
        discard.reach = reach;
        discard.path = path;
    }
}

/// How far a failure could have travelled, and the calls it would take.
///
/// Three separable questions: which callables are above this one, which of
/// them decides the verdict, and what the calls between them are.
fn search_upward(
    start: usize,
    callers: &BTreeMap<usize, Vec<(usize, &Callsite)>>,
    callables: &[CallableNode],
) -> (Reach, Vec<CallEdgeEvidence>) {
    if !callers.contains_key(&start) {
        return (Reach::Unknown, Vec::new());
    }
    // Every callable that reaches the discard, each recorded once with the
    // call below it and how many calls up it sits. Recording the step down
    // rather than a whole chain per entry is what makes any one path
    // rebuildable afterwards, and recording each callable once at its
    // shortest is what terminates on a recursive call graph.
    let above = dijkstra_all(&start, |current| {
        callers
            .get(current)
            .into_iter()
            .flatten()
            .map(|(caller, _)| (*caller, 1usize))
    });

    // The nearest caller that could report it is the one worth naming: it is
    // the smallest change that would let the failure out.
    let reportable = above
        .iter()
        .filter(|(caller, _)| callables[**caller].containment == Containment::Fallible)
        .min_by_key(|(caller, (_, distance))| (*distance, **caller))
        .map(|(caller, _)| *caller);
    if let Some(ancestor) = reportable {
        return (
            Reach::Ancestor,
            chain_to(ancestor, &above, callers, callables),
        );
    }

    // Nothing above can be told, so report how far the failure travels before
    // the calls run out. Ties go to the lowest index, never to search order.
    let furthest = above
        .iter()
        .max_by_key(|(caller, (_, distance))| (*distance, std::cmp::Reverse(**caller)))
        .map(|(caller, _)| *caller);
    let chain = furthest
        .map(|caller| chain_to(caller, &above, callers, callables))
        .unwrap_or_default();
    (Reach::Sealed, chain)
}

/// The calls from `ancestor` down to the discard, outermost call first.
fn chain_to(
    ancestor: usize,
    above: &HashMap<usize, (usize, usize)>,
    callers: &BTreeMap<usize, Vec<(usize, &Callsite)>>,
    callables: &[CallableNode],
) -> Vec<CallEdgeEvidence> {
    build_path(&ancestor, above)
        .windows(2)
        .rev()
        .filter_map(|step| {
            let [callee, caller] = step else {
                return None;
            };
            let (_, call) = callers
                .get(callee)
                .into_iter()
                .flatten()
                .find(|(above, _)| above == caller)?;
            Some(CallEdgeEvidence {
                caller: callables[*caller].path.clone(),
                callee: callables[*callee].path.clone(),
                call: call.span.clone(),
            })
        })
        .collect()
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}

/// The bare type name of an `impl` block's self type, as it appears in a
/// `{module}::{implementation}::{name}` callable path.
///
/// Yields `Bar` for `impl Trait for &mut Bar`, not `&mut Bar`. This crate and
/// `infact-rust-effects` construct the same callable identity from this, so the
/// two must agree; the unqualified spelling wins because a reference type is not
/// usable as a path segment.
fn simple_type_name(value: &str) -> String {
    let base = value.split('<').next().unwrap_or(value);
    base.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .rfind(|part| {
            !part.is_empty()
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        })
        .unwrap_or(value)
        .to_owned()
}

/// Keep a reported expression to one readable line.
fn truncate(value: &str) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= 80 {
        return flattened;
    }
    let kept = flattened.chars().take(77).collect::<String>();
    format!("{kept}...")
}

fn source_span(path: &Path, node: Node<'_>) -> Result<SourceSpan> {
    let too_large = || Error::SourceTooLarge {
        path: path.to_path_buf(),
    };
    Ok(SourceSpan {
        path: path.to_path_buf(),
        start_byte: Some(u64::try_from(node.start_byte()).map_err(|_| too_large())?),
        end_byte: Some(u64::try_from(node.end_byte()).map_err(|_| too_large())?),
        start_line: u32::try_from(node.start_position().row + 1).map_err(|_| too_large())?,
        end_line: u32::try_from(node.end_position().row + 1).map_err(|_| too_large())?,
        start_column: u32::try_from(node.start_position().column + 1).ok(), // straitjacket-allow:error-discard — a column past u32 is reported as absent, as the field allows
        end_column: u32::try_from(node.end_position().column + 1).ok(), // straitjacket-allow:error-discard — a column past u32 is reported as absent, as the field allows
    })
}

fn repository_module(path: &Path) -> String {
    let mut module = path
        .with_extension("")
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
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

fn input_evidence(file: &ParsedFile) -> InputEvidence {
    InputEvidence {
        path: file.path.clone(),
        content_sha256: file.provenance.source_sha256.clone(),
        parser_id: file.provenance.parser_id.clone(),
        parser_version: file.provenance.parser_version.clone(),
        grammar_sha256: file.provenance.grammar_sha256.clone(),
        queries_sha256: file.provenance.queries_sha256.clone(),
    }
}

fn discard_derivation(
    discard: &ErrorDiscard,
    inputs: &BTreeMap<PathBuf, InputEvidence>,
) -> Derivation {
    let paths = BTreeSet::from([discard.span.path.clone()]);
    Derivation {
        analyzer: "infact-errors".to_owned(),
        analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
        inputs: paths
            .into_iter()
            .filter_map(|path| inputs.get(&path).cloned())
            .collect(),
    }
}
