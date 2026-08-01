//! Derives a library callable's behavior by normalizing its implementation.
//!
//! Derivation knows no callable paths. It locates whatever function the catalog
//! names, normalizes its body into the language-neutral form, and follows
//! delegation so that a public wrapper describes the work its helper actually
//! does. One implementation covers every callable in every library.

use std::collections::BTreeSet;
use std::path::Path;

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use infact_core::{
    CallableContainer, DerivedLibraryBehavior, EXTERNAL_CATALOG_SCHEMA, ExternalCallable,
    ExternalCatalog, Form, ImplementationEvidence,
};
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
    /// Whether every inline module enclosing it is public.
    reachable: bool,
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn collect_functions<'a>(
    node: Node<'a>,
    file: &'a ParsedFile,
    container: Option<&str>,
    reachable: bool,
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
            reachable,
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
    // an inline `mod` that is not public hides everything inside it
    let nested_reachable = reachable
        && (node.kind() != "mod_item" || {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .any(|child| child.kind() == "visibility_modifier")
        });
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(child, file, nested, nested_reachable, output);
    }
}

/// Names of modules this library declares without making them reachable.
///
/// A file-based module is declared elsewhere, so whether `src/lexical/math.rs`
/// is reachable cannot be seen from that file. Collecting the declarations
/// first is what makes the answer available where it is needed.
///
/// A private module whose contents are re-exported is still reachable, and
/// libraries lean on that: `mod traits;` beside `pub use self::traits::Iterator;`
/// is how the standard library presents most of its API. Missing that would
/// hide exactly the items worth knowing about.
fn private_modules(files: &[ParsedFile]) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut private = BTreeSet::new();
    let mut public = BTreeSet::new();
    for file in files {
        if file.pack.language().id != "rust" {
            continue;
        }
        collect_module_declarations(file.tree.root_node(), file, &mut private, &mut public);
    }
    // a name made public anywhere is treated as reachable, because suppressing
    // a real API is worse than admitting one that is only sometimes reachable
    private.retain(|name| !public.contains(name));
    (private, public)
}

fn collect_module_declarations(
    node: Node<'_>,
    file: &ParsedFile,
    private: &mut BTreeSet<String>,
    public: &mut BTreeSet<String>,
) {
    // a public re-export makes everything it names reachable, whatever the
    // visibility of the module it came from
    if node.kind() == "use_declaration" {
        let mut cursor = node.walk();
        let exported = node
            .children(&mut cursor)
            .any(|child| child.kind() == "visibility_modifier");
        if exported && let Some(text) = node_text(node, &file.source) {
            public.extend(
                text.split(|character: char| !character.is_alphanumeric() && character != '_')
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    if node.kind() == "mod_item"
        && let Some(name) = node
            .child_by_field_name("name")
            .and_then(|name| node_text(name, &file.source))
    {
        let mut cursor = node.walk();
        let is_public = node
            .children(&mut cursor)
            .any(|child| child.kind() == "visibility_modifier");
        if is_public {
            public.insert(name.to_owned());
        } else {
            private.insert(name.to_owned());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_module_declarations(child, file, private, public);
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
        collect_functions(file.tree.root_node(), file, None, true, &mut functions);
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
    let parsed = parse_repository(source_root, parsers)?;
    let functions = library_functions(&parsed.files);
    derive_from(&functions, catalog, callable_path)
}

/// Derive one behavior from an already parsed library.
///
/// Parsing a crate is the expensive part and it does not depend on which
/// callable is being derived, so deriving a whole library must not repeat it
/// once per callable.
fn derive_from(
    functions: &[LibraryFunction<'_>],
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
        && let Some(implementing) = principal_method(functions, constructed)
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
    if form.depth() > MAXIMUM_FORM_DEPTH {
        return Err(Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: format!(
                "the implementation nests {} levels, which describes a subsystem rather than a behavior",
                form.depth()
            ),
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

/// The deepest form still worth keeping.
///
/// Each level of a form becomes two or three levels of JSON — a tag, a struct,
/// sometimes a list — so a reader that refuses 128 container levels gives up
/// well before a form is that deep. This is set low enough to stay clear of
/// that, and a behavior anywhere near it describes a subsystem rather than an
/// operation.
pub const MAXIMUM_FORM_DEPTH: u32 = 32;

/// The smallest form worth reporting as a match.
///
/// Calibrated against derived behaviors and the code they are matched into.
/// The smallest genuine behavior measured here is seven nodes, while the forms
/// that collide across unrelated code are two or three: a field accessor, a
/// one-line delegation, a struct literal. Anything below this floor describes
/// too little to identify an API.
pub const MINIMUM_REPORTABLE_SIZE: u32 = 6;

/// The least a behavior must name to identify an API rather than a shape.
///
/// Measured against the behaviors that matter: `sorted` names a container and a
/// method, which is two. A traversal that names nothing is every library's
/// `map` and matches everything.
pub const MINIMUM_ANCHORS: u32 = 2;

/// Whether a derived behavior is specific enough to report when matched.
pub fn is_reportable(form: &Form) -> bool {
    form.size() >= MINIMUM_REPORTABLE_SIZE
        && form.anchors() >= MINIMUM_ANCHORS
        && describes_work(form)
}

/// Whether a function is part of a library's public surface.
///
/// A `pub` function is, and so is anything written inside a trait, because a
/// trait's methods are reachable wherever the trait is. This is a syntactic
/// approximation: it does not know whether the enclosing module is itself
/// public, so it errs toward including too much rather than too little.
fn is_public(
    function: &LibraryFunction<'_>,
    private_modules: &BTreeSet<String>,
    exported: &BTreeSet<String>,
) -> bool {
    // A type or trait re-exported by name is reachable however private the
    // module holding it. `pub use self::traits::Iterator` names the trait, not
    // the module, which is how most of the standard library is presented.
    let exported_container = function
        .container
        .as_deref()
        .is_some_and(|container| exported.contains(container));

    // Otherwise a `pub fn` inside a private module is not reachable, and a
    // catalog full of items nobody can call produces behaviors nobody can be
    // advised to use.
    if !exported_container
        && (!function.reachable
            || function
                .file
                .path
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .map(|segment| segment.trim_end_matches(".rs"))
                .any(|segment| private_modules.contains(segment)))
    {
        return false;
    }
    if function.container.is_some()
        && function
            .node
            .parent()
            .and_then(|parent| parent.parent())
            .is_some_and(|item| item.kind() == "trait_item")
    {
        return true;
    }
    let mut cursor = function.node.walk();
    function
        .node
        .children(&mut cursor)
        .any(|child| child.kind() == "visibility_modifier")
}

/// Build a callable catalog from a library's own source.
///
/// The published alternative is rustdoc JSON, which is more precise and costs a
/// nightly toolchain and a successful build of the library. Derivation reads
/// only a callable's path, so source is enough, and a catalog that needs
/// nothing but the source keeps every dependency reachable.
pub fn derive_catalog(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    package: &str,
    version: &str,
) -> Result<ExternalCatalog> {
    let parsed = parse_repository(source_root, parsers)?;
    let functions = library_functions(&parsed.files);
    catalog_from(&parsed, &functions, package, version)
}

/// Everything derived from one library, and what could not be read.
pub struct DerivedLibrary {
    pub catalog: ExternalCatalog,
    pub behaviors: Vec<DerivedLibraryBehavior>,
    /// Files the parser could not read.
    ///
    /// A grammar rejects a whole file when any part of it is beyond what it
    /// knows, so an unsupported item silently removes everything beside it.
    /// Reporting these keeps "this library has no behaviors" apart from "this
    /// library could not be read", which look identical otherwise.
    pub unparsed: Vec<String>,
}

/// Derive a library's whole catalog and every behavior it yields, parsing once.
pub fn derive_library(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    package: &str,
    version: &str,
) -> Result<DerivedLibrary> {
    let parsed = parse_repository(source_root, parsers)?;
    let functions = library_functions(&parsed.files);
    let catalog = catalog_from(&parsed, &functions, package, version)?;
    let behaviors = catalog
        .callables
        .iter()
        // most callables describe no comparable behavior; that is expected
        .filter_map(|callable| derive_from(&functions, &catalog, &callable.path).ok())
        .collect();
    let mut unparsed = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.path.display().to_string())
        .collect::<Vec<_>>();
    unparsed.sort();
    unparsed.dedup();
    Ok(DerivedLibrary {
        catalog,
        behaviors,
        unparsed,
    })
}

fn catalog_from(
    parsed: &entl_tree_sitter::ParsedRepository,
    functions: &[LibraryFunction<'_>],
    package: &str,
    version: &str,
) -> Result<ExternalCatalog> {
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    let mut sources = parsed
        .files
        .iter()
        .filter(|file| is_library_source(&file.path))
        .map(|file| (file.path.clone(), file.provenance.source_sha256.clone()))
        .collect::<Vec<_>>();
    sources.sort();
    for (path, content) in &sources {
        digest.update(path.to_string_lossy().as_bytes());
        digest.update(content.as_bytes());
    }
    let source_sha256 = format!("sha256:{:x}", digest.finalize());

    let (private, exported) = private_modules(&parsed.files);
    let mut callables = functions
        .iter()
        .filter(|function| is_public(function, &private, &exported))
        .map(|function| {
            let path = match &function.container {
                Some(container) => format!("{package}::{container}::{}", function.name),
                None => format!("{package}::{}", function.name),
            };
            let container = match &function.container {
                Some(container) => CallableContainer::Trait {
                    path: format!("{package}::{container}"),
                },
                None => CallableContainer::Module {
                    path: package.to_owned(),
                },
            };
            ExternalCallable {
                path,
                container,
                // source says nothing about types
                signature: None,
            }
        })
        .collect::<Vec<_>>();
    callables.sort();
    callables.dedup();

    Ok(ExternalCatalog {
        schema: EXTERNAL_CATALOG_SCHEMA,
        package: package.to_owned(),
        version: version.to_owned(),
        // zero records that no rustdoc format was involved
        rustdoc_format: 0,
        source_sha256,
        callables,
    })
}
