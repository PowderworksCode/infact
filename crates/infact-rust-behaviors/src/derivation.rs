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
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
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
        | Form::Sift { .. }
        | Form::Transform { .. }
        | Form::Retain { .. }
        | Form::Accumulate { .. }
        | Form::Collect { .. } => true,
        _ => form.children().into_iter().any(describes_work),
    }
}

/// The type a name refers to, with `Self` read as the type it stands for.
///
/// `Self` is not a type name: it means whichever type the surrounding `impl` is
/// for. Taken literally it matches every `impl Self` in the library, so
/// `Arc::from_raw` — whose body is `Self::from_raw_in(..)` — was deriving the
/// behavior of `Vec::from_iter_exact`, and fifty-four other callables were
/// linked to implementations belonging to unrelated types. Where the enclosing
/// type is unknown, there is no answer, and none is better than an arbitrary
/// one.
fn resolve_self<'a>(name: &'a str, container: Option<&'a str>) -> Option<&'a str> {
    if name == "Self" {
        return container;
    }
    Some(name)
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
/// A type that implements none of these is not a lazy adaptor, and there is
/// nothing to follow it into.
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
            let form = normalize(function).ok()?; // straitjacket-allow:error-discard — a function that will not normalize is not a candidate
            // The method the language requires wins even when its body is a
            // single call, because that call is followed afterwards. Demanding
            // that it already describe work discards `fn next(&mut self) {
            // self.iter.find_map(&mut self.f) }` and leaves a bulk
            // specialisation to win by default, which describes something the
            // caller never asked for.
            let principal = PRINCIPAL_METHODS.contains(&function.name.as_str());
            let works = describes_work(&form);
            // `next` is the contract; `next_back` and the rest are
            // specializations of it. Ranking by size alone let `next_back` win
            // whenever `next` was a one-line delegation, which is exactly when
            // `next` is the method worth following.
            let standing = PRINCIPAL_METHODS
                .iter()
                .position(|candidate| *candidate == function.name)
                .map_or(0, |rank| PRINCIPAL_METHODS.len() - rank);
            // Only a method the language requires can stand for a type's
            // behavior. Admitting any function that merely does work meant a
            // type with no principal method at all — `Arc`, `HashMap` — handed
            // back whichever of its methods happened to be largest, so
            // `Arc::from_raw` derived the behavior of an unrelated function.
            // A type with no contract method has no implementation to follow.
            principal.then_some(((standing, works, form.size()), function))
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

    // Follow the implementation until it describes actual work, by whichever
    // route leads there: a wrapper delegating to a helper, or a callable that
    // only builds the type whose implementation does the work later.
    for _ in 0..MAX_DELEGATION_DEPTH {
        if describes_work(&form) {
            break;
        }
        let next = delegation_target(&form)
            .and_then(|target| resolve(target, current.container.as_deref(), Some(current)))
            .or_else(|| {
                let built = constructed_type(&form)?;
                let built = resolve_self(built, current.container.as_deref())?;
                principal_method(functions, built)
            });
        let Some(next) = next else {
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
        && let Some(constructed) = resolve_self(constructed, current.container.as_deref())
        && let Some(implementing) = principal_method(functions, constructed)
    {
        form = normalize(implementing)?;
        implementation.push(evidence(implementing)?);
    }

    if !is_comparable(&form) {
        return Err(Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: "the implementation describes nothing that can be compared".to_owned(),
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
    // The laws of iteration run here rather than at the end, because what an
    // implementation *does* is only visible in normal form: `find` describes no
    // sequence operation until its fold has become a traversal, and the search
    // for delegation would give up on it before ever seeing the work.
    Ok(
        infact_rust_normalize::normalize_function(function.node, &function.file.source)
            .simplify(),
    )
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

/// Whether a form describes something that can be compared across libraries.
///
/// Derivation used to demand a sequence operation, which confined it to
/// iterator behaviors and rejected everything else a library does. What
/// actually has to hold is that the form describes a *decision or a traversal*
/// rather than plumbing: iterating over something, or choosing among named
/// alternatives. A getter, a delegation, or a struct literal describes neither,
/// and would collide with unrelated code wherever it appeared.
pub fn is_comparable(form: &Form) -> bool {
    describes_work(form) || describes_decision(form)
}

/// Whether a form chooses among alternatives it names.
///
/// One arm is not a decision — `match x { Some(v) => v }` says only that a
/// value was unwrapped, which most code does somewhere. Two named alternatives
/// is the point at which the shape belongs to a particular type's API.
fn describes_decision(form: &Form) -> bool {
    if let Form::Select { arms, .. } = form {
        let named = arms
            .iter()
            .filter_map(|arm| match &arm.pattern {
                infact_core::Pattern::Variant { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if named.len() >= 2 {
            return true;
        }
    }
    form.children().into_iter().any(describes_decision)
}

/// Whether a derived behavior is specific enough to report when matched.
///
/// The last condition is what separates a behavior from a shape. `Option::map_or`
/// is `match self { Some(t) => f(t), None => default }`: two named alternatives,
/// and everything else a hole. It therefore describes *every* way of consuming
/// an `Option`, subsumes the narrower behaviors, and reported nine hundred times
/// across five hundred crates — technically right and useless. `unwrap_or` says
/// the same thing about `None` but is concrete about `Some`, and stays.
///
/// So a behavior must name at least as much as it leaves open. This is a
/// property of the form rather than a threshold chosen to make a number look
/// good: a form with more holes than anchors matches more situations than it
/// distinguishes.
pub fn is_reportable(form: &Form) -> bool {
    form.size() >= MINIMUM_REPORTABLE_SIZE
        && form.anchors() >= MINIMUM_ANCHORS
        && form.anchors() >= form.holes()
        && is_comparable(form)
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
    /// Why the callables that produced nothing produced nothing, counted.
    ///
    /// Most of a library is not a comparable behavior, so a small behavior list
    /// is the normal outcome rather than a fault. What is worth knowing is the
    /// shape of what was skipped: whether coverage is limited by the language
    /// constructs derivation understands, by what it is willing to call a
    /// behavior, or by something going wrong. Counting the reasons is the only
    /// way to tell those apart without reading every callable.
    pub skipped: std::collections::BTreeMap<String, usize>,
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
    // Most callables describe no comparable behavior, which is expected and
    // skipped. A parse or source failure on the same path is not, and would
    // otherwise leave a short behavior list that reads like a small library.
    let mut behaviors = Vec::new();
    let mut skipped = std::collections::BTreeMap::new();
    for callable in &catalog.callables {
        match derive_from(&functions, &catalog, &callable.path) {
            Ok(behavior) => behaviors.push(behavior),
            Err(error) if error.is_underivable() => {
                *skipped.entry(skip_reason(&error)).or_insert(0) += 1;
            }
            Err(error) => return Err(error),
        }
    }
    let mut unparsed = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.path.display(), diagnostic.message))
        .collect::<Vec<_>>();
    unparsed.sort();
    unparsed.dedup();
    Ok(DerivedLibrary {
        catalog,
        behaviors,
        unparsed,
        skipped,
    })
}

/// The reason a callable yielded nothing, with the callable's own name removed.
///
/// The name is what makes each message unique, and counting unique messages
/// says nothing. What is wanted is the handful of reasons behind thousands of
/// skips.
fn skip_reason(error: &Error) -> String {
    match error {
        Error::UnsupportedImplementation { reason, .. } => reason
            .split(", which describes")
            .next()
            .unwrap_or(reason)
            .to_owned(),
        Error::UnsupportedDerivation { .. } => "the callable is not a function".to_owned(),
        Error::MissingCallable { .. } => "the catalog names no such callable".to_owned(),
        Error::IncompatibleCallable { .. } => "the callable cannot be compared".to_owned(),
        Error::MissingImplementation { .. } => "no implementation was found".to_owned(),
        Error::UnsupportedMacroExpansion { .. } => "the macro cannot be expanded".to_owned(),
        other => other.to_string(),
    }
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
