//! Derives a library callable's behavior by normalizing its implementation.
//!
//! Derivation knows no callable paths. It locates whatever function the catalog
//! names, normalizes its body into the language-neutral form, and follows
//! delegation so that a public wrapper describes the work its helper actually
//! does. One implementation covers every callable in every library.

use std::path::Path;

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use infact_core::{DerivedLibraryBehavior, ExternalCatalog, Form, ImplementationEvidence};
use tree_sitter::Node;

use crate::{DERIVED_LIBRARY_BEHAVIOR_SCHEMA, Error, Result, source_sha256, span_of};

/// How many delegating wrappers to follow before giving up.
const MAX_DELEGATION_DEPTH: usize = 4;

/// A function found in the library source.
struct LibraryFunction<'a> {
    file: &'a ParsedFile,
    node: Node<'a>,
    name: String,
    /// The trait or type the function is written inside, when there is one.
    container: Option<String>,
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn collect_functions<'a>(
    node: Node<'a>,
    file: &'a ParsedFile,
    container: Option<&str>,
    output: &mut Vec<LibraryFunction<'a>>,
) {
    if node.kind() == "function_item"
        && let Some(name) = node
            .child_by_field_name("name")
            .and_then(|name| node_text(name, &file.source))
    {
        output.push(LibraryFunction {
            file,
            node,
            name: name.to_owned(),
            container: container.map(str::to_owned),
        });
    }
    // an `impl` names a type, a `trait` names itself; either qualifies the
    // functions written inside it
    let nested = match node.kind() {
        "impl_item" => node
            .child_by_field_name("type")
            .and_then(|ty| node_text(ty, &file.source))
            .map(bare_type_name),
        "trait_item" => node
            .child_by_field_name("name")
            .and_then(|name| node_text(name, &file.source)),
        _ => container,
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(child, file, nested, output);
    }
}

/// A type name without its generic arguments, so `impl Iterator for MapInto<I>`
/// is recognized as describing `MapInto`.
fn bare_type_name(name: &str) -> &str {
    name.split_once('<').map_or(name, |(bare, _)| bare).trim()
}

/// Whether a path is the library's own source rather than its tests, benchmarks,
/// or examples.
///
/// Cargo gives those directories a fixed meaning, so this is a language
/// convention rather than a fact about any particular crate. It matters because
/// test suites routinely define functions named after the API they exercise.
fn is_library_source(path: &Path) -> bool {
    let mut components = path.components().map(|component| component.as_os_str());
    !components.any(|component| {
        matches!(
            component.to_str(),
            Some("tests" | "benches" | "examples" | "target")
        )
    })
}

fn library_functions(files: &[ParsedFile]) -> Vec<LibraryFunction<'_>> {
    let mut functions = Vec::new();
    for file in files {
        if file.pack.language().id != "rust" || !is_library_source(&file.path) {
            continue;
        }
        collect_functions(file.tree.root_node(), file, None, &mut functions);
    }
    functions
}

/// The name a callable path ends in.
fn leaf_name(callable_path: &str) -> &str {
    callable_path.rsplit("::").next().unwrap_or(callable_path)
}

/// The trait or type a callable path is qualified by, when it has one.
fn container_name(callable_path: &str) -> Option<&str> {
    let mut segments = callable_path.rsplit("::");
    segments.next()?;
    segments
        .next()
        .filter(|segment| segment.starts_with(|first: char| first.is_ascii_uppercase()))
}

/// Whether a form is nothing but a call to somewhere else.
///
/// A public API is frequently a one-line wrapper: `counts` exists to call
/// `counts_with_hasher` with a default. The wrapper describes no behavior of
/// its own, so derivation follows it. This is a shape test, not a list of known
/// wrappers.
fn delegation_target(form: &Form) -> Option<&str> {
    match form {
        Form::Method { name, .. } => Some(name),
        Form::Call { callee, .. } => match callee.as_ref() {
            Form::Path(path) => Some(leaf_name(path)),
            _ => None,
        },
        Form::Return(inner) => delegation_target(inner),
        Form::Sequence(parts) => match parts.as_slice() {
            [only] => delegation_target(only),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a form describes work rather than plumbing.
fn describes_work(form: &Form) -> bool {
    match form {
        Form::Traverse { .. }
        | Form::Transform { .. }
        | Form::Retain { .. }
        | Form::Accumulate { .. }
        | Form::Collect { .. } => true,
        _ => form.children().into_iter().any(describes_work),
    }
}

/// The type a form does nothing but construct.
fn constructed_type(form: &Form) -> Option<&str> {
    match form {
        Form::Construct(name) => Some(name),
        Form::Return(inner) => constructed_type(inner),
        Form::Sequence(parts) => match parts.as_slice() {
            [only] => constructed_type(only),
            _ => None,
        },
        // `MapInto { iter: self }` and similar struct literals reach here as
        // opaque syntax; the constructed type is whatever the parts construct
        _ => form.children().into_iter().find_map(constructed_type),
    }
}

/// Methods that define what a type is, rather than merely optimizing it.
///
/// An iterator's `next` is its whole contract; its `fold` and `size_hint` are
/// specializations of that contract. These are the language's own trait
/// requirements, so the list holds for every library.
const PRINCIPAL_METHODS: &[&str] = &["next", "next_back", "poll", "poll_next"];

/// The method that carries a type's behavior.
///
/// A type's work is spread across its implementations and most of those methods
/// are bookkeeping. Prefer the one the language requires; failing that, take
/// whichever describes the most work. This is a good enough answer, because a
/// finding here is a prompt to look rather than a proof.
fn principal_method<'a>(
    functions: &'a [LibraryFunction<'a>],
    type_name: &str,
) -> Option<&'a LibraryFunction<'a>> {
    functions
        .iter()
        .filter(|function| function.container.as_deref() == Some(type_name))
        .filter_map(|function| {
            let form = normalize(function).ok()?;
            describes_work(&form).then(|| {
                let principal = PRINCIPAL_METHODS.contains(&function.name.as_str());
                ((principal, form.size()), function)
            })
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, function)| function)
}

fn evidence(function: &LibraryFunction<'_>) -> Result<ImplementationEvidence> {
    Ok(ImplementationEvidence {
        callable_path: function.name.clone(),
        span: span_of(function.file, function.node)?,
        source_sha256: source_sha256(&function.file.source),
    })
}

/// Derive the behavior of `callable_path` from a library checkout.
pub fn derive_behavior(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
    callable_path: &str,
) -> Result<DerivedLibraryBehavior> {
    let callable = catalog
        .callables
        .iter()
        .find(|callable| callable.path == callable_path)
        .ok_or_else(|| Error::MissingCallable {
            callable: callable_path.to_owned(),
        })?;

    let parsed = parse_repository(source_root, parsers)?;
    let functions = library_functions(&parsed.files);
    // Resolve a name to one implementation, preferring the container the
    // catalog qualified the callable with. Without a container to go on, an
    // ambiguous name cannot be resolved by syntax alone.
    // `exclude` drops the function doing the delegating: a wrapper and the
    // helper it forwards to routinely share a name across a trait and a module,
    // and the wrapper is never its own implementation.
    let resolve = |name: &str, container: Option<&str>, exclude: Option<&LibraryFunction<'_>>| {
        let candidates = || {
            functions.iter().filter(move |function| {
                function.name == name
                    && exclude.is_none_or(|excluded| !std::ptr::eq(*function, excluded))
            })
        };
        if let Some(container) = container {
            let mut qualified =
                candidates().filter(|function| function.container.as_deref() == Some(container));
            if let Some(first) = qualified.next()
                && qualified.next().is_none()
            {
                return Some(first);
            }
        }
        let mut matching = candidates();
        let first = matching.next()?;
        matching.next().is_none().then_some(first)
    };

    let mut current = resolve(
        leaf_name(&callable.path),
        container_name(&callable.path),
        None,
    )
    .ok_or_else(|| Error::MissingImplementation {
        callable: callable_path.to_owned(),
    })?;
    let mut implementation = vec![evidence(current)?];
    let mut form = normalize(current)?;
    let mut visited = vec![std::ptr::from_ref(current)];

    // Follow delegation until the form describes actual work.
    for _ in 0..MAX_DELEGATION_DEPTH {
        if describes_work(&form) {
            break;
        }
        let Some(target) = delegation_target(&form) else {
            break;
        };
        // a delegate is looked up in the same container first
        let Some(next) = resolve(target, current.container.as_deref(), Some(current)) else {
            break;
        };
        // a trait wrapper and the free function it forwards to commonly share
        // a name, so identity rather than name decides whether this is a cycle
        if visited.contains(&std::ptr::from_ref(next)) {
            break;
        }
        visited.push(std::ptr::from_ref(next));
        current = next;
        form = normalize(current)?;
        implementation.push(evidence(current)?);
    }

    // A combinator does not do its work where it is called. `map_into` only
    // builds a `MapInto`, and the behavior lives in that type's `Iterator`
    // implementation, which runs later. When a callable just constructs
    // something, the type it constructs is where to look.
    if !describes_work(&form)
        && let Some(constructed) = constructed_type(&form)
        && let Some(implementing) = principal_method(&functions, constructed)
    {
        form = normalize(implementing)?;
        implementation.push(evidence(implementing)?);
    }

    if !describes_work(&form) {
        return Err(Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: "the implementation describes no sequence operation to compare".to_owned(),
        });
    }

    Ok(DerivedLibraryBehavior {
        schema: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
        callable_package: catalog.package.clone(),
        callable_version: catalog.version.clone(),
        callable_path: callable.path.clone(),
        catalog_sha256: catalog.source_sha256.clone(),
        implementation,
        program: form,
    })
}

fn normalize(function: &LibraryFunction<'_>) -> Result<Form> {
    if function.node.child_by_field_name("body").is_none() {
        return Err(Error::UnsupportedImplementation {
            callable: function.name.clone(),
            reason: "the implementation has no body".to_owned(),
        });
    }
    Ok(infact_rust_normalize::normalize_function(
        function.node,
        &function.file.source,
    ))
}

/// The smallest form worth reporting as a match.
///
/// Calibrated against derived behaviors and the code they are matched into.
/// The smallest genuine behavior measured here is seven nodes, while the forms
/// that collide across unrelated code are two or three: a field accessor, a
/// one-line delegation, a struct literal. Anything below this floor describes
/// too little to identify an API.
pub const MINIMUM_REPORTABLE_SIZE: u32 = 6;

/// Whether a derived behavior is specific enough to report when matched.
pub fn is_reportable(form: &Form) -> bool {
    form.size() >= MINIMUM_REPORTABLE_SIZE && describes_work(form)
}
