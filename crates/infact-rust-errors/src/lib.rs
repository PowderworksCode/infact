//! Discarded-error facts derived from Rust source.
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use infact_core::{
    CallEdgeEvidence, Certainty, Containment, Derivation, DiscardForm, ErrorDiscard, Fact,
    InputEvidence, Reach, SourceSpan,
};
use pathfinding::directed::dijkstra::{build_path, dijkstra_all};
use tree_sitter::Node;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("loading Rust parser pack: {0}")]
    Parser(#[from] entl_tree_sitter::Error),
    #[error("source file {} is too large for source coordinates", path.display())]
    SourceTooLarge { path: PathBuf },
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

/// Find every discarded-error site in a repository's Rust source.
pub fn analyze_repository_errors(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
) -> Result<RepositoryErrorReport> {
    let parsed = parse_repository(root, parsers)?;
    let mut discards = Vec::new();
    let mut callables = Vec::new();
    let mut inputs = BTreeMap::new();
    for file in &parsed.files {
        if file.pack.language().id != "rust" {
            continue;
        }
        inputs.insert(file.path.clone(), input_evidence(file));
        let facts = collect_file(file)?;
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
pub fn analyze_file(file: &ParsedFile) -> Result<Vec<ErrorDiscard>> {
    let mut facts = collect_file(file)?;
    resolve_reach(&mut facts.discards, &facts.callables);
    facts.discards.sort();
    facts.discards.dedup();
    Ok(facts.discards)
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

fn collect_file(file: &ParsedFile) -> Result<FileFacts> {
    let module = repository_module(&file.path);
    let mut facts = FileFacts {
        discards: Vec::new(),
        callables: Vec::new(),
    };
    let scope = Scope {
        callable: module.clone(),
        span: source_span(&file.path, file.tree.root_node())?,
        containment: Containment::Infallible,
        in_test: false,
        index: None,
    };
    walk(
        file.tree.root_node(),
        &file.source,
        &file.path,
        &module,
        None,
        &scope,
        &mut facts,
    )?;
    Ok(facts)
}

#[derive(Debug, Clone)]
struct Scope {
    callable: String,
    span: SourceSpan,
    containment: Containment,
    in_test: bool,
    /// Where this callable sits in `FileFacts::callables`, if it is one.
    index: Option<usize>,
}

/// Walk the tree, tracking the nearest enclosing callable.
///
/// A discard is attributed to the callable that contains it, because that is
/// the one whose signature decided whether the error had anywhere to go.
fn walk(
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    module: &str,
    implementation: Option<String>,
    scope: &Scope,
    facts: &mut FileFacts,
) -> Result<()> {
    match node.kind() {
        "impl_item" => {
            let implementation = node
                .child_by_field_name("type")
                .and_then(|node| node_text(node, source))
                .map(simple_type_name);
            return walk_children(node, source, path, module, implementation, scope, facts);
        }
        "function_item" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|node| node_text(node, source))
            else {
                return Ok(());
            };
            let callable = match &implementation {
                Some(implementation) => format!("{module}::{implementation}::{name}"),
                None => format!("{module}::{name}"),
            };
            let containment = containment_of(node, source);
            facts.callables.push(CallableNode {
                path: callable.clone(),
                name: name.to_owned(),
                containment,
                calls: Vec::new(),
            });
            let inner = Scope {
                callable,
                span: source_span(path, node)?,
                containment,
                in_test: scope.in_test || has_test_attribute(node, source),
                index: Some(facts.callables.len() - 1),
            };
            if let Some(body) = node.child_by_field_name("body") {
                walk_children(body, source, path, module, implementation, &inner, facts)?;
            }
            return Ok(());
        }
        "mod_item" => {
            let inner = Scope {
                in_test: scope.in_test || is_test_module(node, source),
                ..scope.clone()
            };
            return walk_children(node, source, path, module, implementation, &inner, facts);
        }
        _ => {}
    }

    record_call(node, source, path, scope, facts)?;
    inspect(node, source, path, scope, facts)?;
    walk_children(node, source, path, module, implementation, scope, facts)
}

/// Record a call edge from the enclosing callable, for reach resolution.
fn record_call(
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    scope: &Scope,
    facts: &mut FileFacts,
) -> Result<()> {
    if node.kind() != "call_expression" {
        return Ok(());
    }
    let Some(index) = scope.index else {
        return Ok(());
    };
    let Some(callee) = node.child_by_field_name("function").and_then(|function| {
        let named = match function.kind() {
            // `helper(..)`
            "identifier" => node_text(function, source),
            // `module::helper(..)` and `Type::helper(..)`
            "scoped_identifier" => function
                .child_by_field_name("name")
                .and_then(|name| node_text(name, source)),
            // `receiver.helper(..)`
            "field_expression" => function
                .child_by_field_name("field")
                .and_then(|field| node_text(field, source)),
            _ => None,
        };
        named.map(str::to_owned)
    }) else {
        return Ok(());
    };
    let span = source_span(path, node)?;
    facts.callables[index].calls.push(Callsite { callee, span });
    Ok(())
}

fn walk_children(
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    module: &str,
    implementation: Option<String>,
    scope: &Scope,
    facts: &mut FileFacts,
) -> Result<()> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk(
            child,
            source,
            path,
            module,
            implementation.clone(),
            scope,
            facts,
        )?;
    }
    Ok(())
}

/// Recognize one node as a discard, if it is one.
fn inspect(
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    scope: &Scope,
    facts: &mut FileFacts,
) -> Result<()> {
    let found = match node.kind() {
        "let_declaration" => let_declaration(node, source),
        "call_expression" => method_call(node, source),
        "match_arm" => err_arm(node, source),
        "let_condition" => let_condition(node, source),
        _ => None,
    };
    let Some((form, certainty, expression)) = found else {
        return Ok(());
    };
    facts.discards.push(ErrorDiscard {
        callable: scope.callable.clone(),
        callable_span: scope.span.clone(),
        form,
        containment: scope.containment,
        certainty,
        expression: truncate(&expression),
        span: source_span(path, node)?,
        in_test: scope.in_test,
        reach: Reach::Unknown,
        path: Vec::new(),
    });
    Ok(())
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

type Recognized = (DiscardForm, Certainty, String);

/// Methods whose `Err` is an answer rather than a failure.
///
/// `binary_search` returns `Err(insertion_point)` to say "not present", and
/// `Path::strip_prefix` returns `Err` to say "not under this prefix". Both are
/// ordinary query outcomes, so discarding them discards nothing.
const QUERY_RESULTS: &[&str] = &[
    "binary_search",
    "binary_search_by",
    "binary_search_by_key",
    "strip_prefix",
];

/// Whether an expression's `Result` reports a query outcome, not a failure.
fn is_query_result(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    node.child_by_field_name("function")
        .filter(|function| function.kind() == "field_expression")
        .and_then(|function| function.child_by_field_name("field"))
        .and_then(|field| node_text(field, source))
        .is_some_and(|method| QUERY_RESULTS.contains(&method))
}

/// `let _ = fallible();` and `let Ok(v) = fallible() else { .. };`
fn let_declaration(node: Node<'_>, source: &[u8]) -> Option<Recognized> {
    let value = node.child_by_field_name("value")?;
    let pattern = node.child_by_field_name("pattern");
    // `_` is an anonymous token, so a wildcard binding fills the pattern field
    // with a node whose kind is the literal underscore rather than leaving it
    // empty. Only a call can carry an error worth reporting.
    if pattern.is_none_or(|pattern| pattern.kind() == "_") {
        // The type is not named here, but `unused_must_use` is what makes this
        // binding necessary at all, so the discard itself is explicit.
        return (value.kind() == "call_expression" || value.kind() == "try_expression").then(
            || {
                (
                    DiscardForm::LetUnderscore,
                    Certainty::Certain,
                    node_text(value, source).unwrap_or_default().to_owned(),
                )
            },
        );
    }
    if is_query_result(value, source) {
        return None;
    }
    // `let Ok(..) = .. else { .. }` — the else arm never sees the error.
    (node.child_by_field_name("alternative").is_some()
        && tuple_struct_name(pattern?, source).as_deref() == Some("Ok"))
    .then(|| {
        (
            DiscardForm::OkBinding,
            Certainty::Certain,
            node_text(value, source).unwrap_or_default().to_owned(),
        )
    })
}

/// `if let Ok(..) = fallible()`, where no arm reads the error.
fn let_condition(node: Node<'_>, source: &[u8]) -> Option<Recognized> {
    let pattern = node.child_by_field_name("pattern")?;
    let value = node.child_by_field_name("value")?;
    if is_query_result(value, source) {
        return None;
    }
    (tuple_struct_name(pattern, source).as_deref() == Some("Ok")).then(|| {
        (
            DiscardForm::OkBinding,
            Certainty::Certain,
            node_text(value, source).unwrap_or_default().to_owned(),
        )
    })
}

/// `Err(_) => ..` — an arm that matches the failure and reads nothing from it.
fn err_arm(node: Node<'_>, source: &[u8]) -> Option<Recognized> {
    let pattern = node.child_by_field_name("pattern")?;
    let mut cursor = pattern.walk();
    let inner = pattern.named_children(&mut cursor).next()?;
    if inner.kind() != "tuple_struct_pattern"
        || node_text(inner.child_by_field_name("type")?, source).map(trailing_segment)
            != Some("Err".to_owned())
    {
        return None;
    }
    // `Err(error)` binds the cause; only a wildcard drops it, and `_` is
    // anonymous, so a binding shows up as a named child beyond the type.
    let type_node = inner.child_by_field_name("type");
    let mut inner_cursor = inner.walk();
    let binds = inner
        .named_children(&mut inner_cursor)
        .any(|child| Some(child) != type_node);
    (!binds).then(|| {
        (
            DiscardForm::ErrArm,
            Certainty::Certain,
            node_text(pattern, source).unwrap_or_default().to_owned(),
        )
    })
}

/// Method calls that drop, downgrade, or erase an error.
fn method_call(node: Node<'_>, source: &[u8]) -> Option<Recognized> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "field_expression" {
        return None;
    }
    let method = node_text(function.child_by_field_name("field")?, source)?;
    let receiver = function.child_by_field_name("value")?;
    let arguments = node.child_by_field_name("arguments");
    let text = node_text(receiver, source).unwrap_or_default().to_owned();

    let recognized = match method {
        // `Result::ok` has no `Option` counterpart, so the receiver is a Result.
        "ok" if is_query_result(receiver, source) => return None,
        "ok" => (DiscardForm::OkDiscard, Certainty::Certain),
        // These read identically on `Option`; the receiver's type is unresolved.
        "unwrap_or" | "unwrap_or_default" => (DiscardForm::UnwrapOr, Certainty::Possible),
        "unwrap_or_else" => {
            // `unwrap_or_else(|error| ..)` still sees the cause.
            if arguments.and_then(first_closure).is_some_and(closure_binds) {
                return None;
            }
            (DiscardForm::UnwrapOr, Certainty::Possible)
        }
        "map_err" => {
            // A `map_err` with no closure at all forwards a function that may
            // well read the cause, so only an explicit `|_|` counts.
            if arguments.and_then(first_closure).is_none_or(closure_binds) {
                return None;
            }
            (DiscardForm::CauseErased, Certainty::Certain)
        }
        "unwrap" | "expect" => (DiscardForm::Panic, Certainty::Possible),
        "filter_map" => {
            let drops = arguments.is_some_and(|arguments| {
                node_text(arguments, source).is_some_and(|text| {
                    let text = text.replace(char::is_whitespace, "");
                    text.contains("Result::ok") || text.ends_with(".ok())")
                })
            });
            if !drops {
                return None;
            }
            (DiscardForm::IteratorDrop, Certainty::Certain)
        }
        _ => return None,
    };
    Some((recognized.0, recognized.1, text))
}

/// The type name of a `Name(..)` pattern, looking through `match_pattern`.
fn tuple_struct_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let node = if node.kind() == "match_pattern" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).next()?
    } else {
        node
    };
    if node.kind() != "tuple_struct_pattern" {
        return None;
    }
    node_text(node.child_by_field_name("type")?, source).map(trailing_segment)
}

fn first_closure(arguments: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .find(|child| child.kind() == "closure_expression")
}

/// Whether a closure binds its parameter, rather than taking `|_|`.
fn closure_binds(closure: Node<'_>) -> bool {
    let Some(parameters) = closure.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = parameters.walk();
    parameters.named_children(&mut cursor).next().is_some()
}

/// Whether the callable can report a failure to its caller.
fn containment_of(node: Node<'_>, source: &[u8]) -> Containment {
    let Some(return_type) = node.child_by_field_name("return_type") else {
        return Containment::Infallible;
    };
    let Some(text) = node_text(return_type, source) else {
        return Containment::Infallible;
    };
    let leading = text.split('<').next().unwrap_or(text);
    if leading.contains("Result") {
        Containment::Fallible
    } else if leading.contains("Option") {
        Containment::Optional
    } else {
        Containment::Infallible
    }
}

fn has_test_attribute(node: Node<'_>, source: &[u8]) -> bool {
    attributes(node, source).any(|text| text.contains("test"))
}

fn is_test_module(node: Node<'_>, source: &[u8]) -> bool {
    attributes(node, source).any(|text| text.replace(char::is_whitespace, "").contains("cfg(test)"))
}

/// The attribute text attached to an item, whether inside it or above it.
fn attributes<'a>(node: Node<'a>, source: &'a [u8]) -> impl Iterator<Item = &'a str> {
    let mut cursor = node.walk();
    let own = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "attribute_item")
        .filter_map(|child| node_text(child, source))
        .collect::<Vec<_>>();
    let mut preceding = Vec::new();
    let mut sibling = node.prev_named_sibling();
    while let Some(current) = sibling {
        if current.kind() != "attribute_item" {
            break;
        }
        if let Some(text) = node_text(current, source) {
            preceding.push(text);
        }
        sibling = current.prev_named_sibling();
    }
    own.into_iter().chain(preceding)
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}

fn trailing_segment(value: &str) -> String {
    value.rsplit("::").next().unwrap_or(value).to_owned()
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
        analyzer: "infact-rust-errors".to_owned(),
        analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
        inputs: paths
            .into_iter()
            .filter_map(|path| inputs.get(&path).cloned())
            .collect(),
    }
}
