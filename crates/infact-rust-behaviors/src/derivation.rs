//! Derives a library callable's behavior by normalizing its implementation.
//!
//! Derivation knows no callable paths. It locates whatever function the catalog
//! names, normalizes its body into the language-neutral form, and follows
//! delegation so that a public wrapper describes the work its helper actually
//! does. One implementation covers every callable in every library.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use infact_behaviors::{Library, LibraryCallable, Refusal, SourceId, container_name, leaf_name};
use infact_core::{
    CallableContainer, DerivedLibraryBehavior, EXTERNAL_CATALOG_SCHEMA, ExternalCallable,
    ExternalCatalog, Form, ImplementationEvidence,
};
use tree_sitter::Node;

use crate::{DERIVED_LIBRARY_BEHAVIOR_SCHEMA, Error, Result, source_sha256, span_of};

/// The methods this language requires of a type that can be followed into.
///
/// An iterator's `next` is its whole contract; its `fold` and `size_hint` are
/// specializations of that contract. These are the language's own trait
/// requirements, so the list holds for every library — and it is the ONE thing
/// about the walk that differs by language, which is why the neutral crate
/// takes it rather than holding it. Earlier entries outrank later ones: ranking
/// by size alone let `next_back` win whenever `next` was a one-line delegation,
/// which is exactly when `next` is the method worth following.
const CONTRACT_METHODS: &[&str] = &["next", "next_back", "poll", "poll_next"];

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

/// Which sources declare each named type.
///
/// A bare type name only identifies a type when the library gives it to one.
/// Counting the files that *implement* a name is not the same question and gets
/// it wrong for a type declared with no `impl` beside it.
type TypeDeclarations = BTreeMap<String, BTreeSet<SourceId>>;

fn type_declarations(
    files: &[ParsedFile],
    source_of: &BTreeMap<PathBuf, SourceId>,
) -> TypeDeclarations {
    let mut declarations = TypeDeclarations::new();
    for file in files {
        if file.pack.language().id != "rust" || !is_library_source(&file.path) {
            continue;
        }
        let Some(source) = source_of.get(&file.path) else {
            continue;
        };
        collect_type_declarations(file.tree.root_node(), file, *source, &mut declarations);
    }
    declarations
}

fn collect_type_declarations(
    node: Node<'_>,
    file: &ParsedFile,
    source: SourceId,
    output: &mut TypeDeclarations,
) {
    if matches!(
        node.kind(),
        "struct_item" | "enum_item" | "union_item" | "type_item"
    ) && let Some(name) = node
        .child_by_field_name("name")
        .and_then(|name| node_text(name, &file.source))
    {
        output.entry(name.to_owned()).or_default().insert(source);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_declarations(child, file, source, output);
    }
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
    let source_of = sources(&parsed.files);
    let callables = callables(&functions, &source_of);
    let library = Library::new(
        &callables,
        type_declarations(&parsed.files, &source_of),
        CONTRACT_METHODS,
    );
    derive_from(&functions, &library, catalog, callable_path)
}

/// Which source each file is, so the walk can ask whether two callables were
/// written in the same place without being handed a filesystem.
fn sources(files: &[ParsedFile]) -> BTreeMap<PathBuf, SourceId> {
    files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.path.clone(), index))
        .collect()
}

/// The normalized bodies the neutral walk reads, in the order found.
///
/// Every function is normalized once, up front. The walk asks for a form
/// repeatedly — ranking a type's methods reads all of them — and normalizing on
/// demand meant doing that work once per question.
///
/// ORDER IS LOAD-BEARING. `principal_method` breaks ties with `max_by_key`,
/// which returns the LAST maximum, so which of two equally-ranked methods wins
/// is decided by the order they arrive in. This is the order `library_functions`
/// produced, which is the order the previous implementation iterated.
fn callables(
    functions: &[LibraryFunction<'_>],
    source_of: &BTreeMap<PathBuf, SourceId>,
) -> Vec<LibraryCallable> {
    functions
        .iter()
        .map(|function| LibraryCallable {
            name: function.name.clone(),
            container: function.container.clone(),
            source: source_of.get(&function.file.path).copied().unwrap_or(0),
            // A signature with no body says what may be called, not what
            // happens. It must stay distinct from a name that resolved to
            // nothing: those are different tallies, and one of them is the
            // 1,332 unexamined std callables.
            form: function
                .node
                .child_by_field_name("body")
                .map(|_| normalize(function)),
        })
        .collect()
}

/// The refusal a caller sees, with the callable it was asked about.
fn refused(refusal: &Refusal, callable_path: &str) -> Error {
    match refusal {
        Refusal::NoImplementation => Error::MissingImplementation {
            callable: callable_path.to_owned(),
        },
        Refusal::NoBody => Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: "the implementation has no body".to_owned(),
        },
        Refusal::NotComparable => Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: "the implementation describes nothing that can be compared".to_owned(),
        },
        Refusal::TooDeep(depth) => Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: format!(
                "the implementation nests {depth} levels, which describes a subsystem rather than a behavior"
            ),
        },
    }
}

/// Derive one behavior from an already parsed library.
///
/// The walk itself is [`infact_behaviors::Library::derive`], which knows no
/// language. What is left here is turning the chain it followed into evidence
/// with spans and digests, which is the part that needs the syntax tree.
fn derive_from(
    functions: &[LibraryFunction<'_>],
    library: &Library<'_>,
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
    let derived = library
        .derive(leaf_name(&callable.path), container_name(&callable.path))
        .map_err(|refusal| refused(&refusal, callable_path))?;

    Ok(DerivedLibraryBehavior {
        schema: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
        callable_package: catalog.package.clone(),
        callable_version: catalog.version.clone(),
        callable_path: callable.path.clone(),
        catalog_sha256: catalog.source_sha256.clone(),
        implementation: derived
            .chain
            .iter()
            .map(|step| evidence(&functions[*step]))
            .collect::<Result<Vec<_>>>()?,
        program: derived.form,
    })
}

/// Whether a form describes something that can be compared across libraries.
///
/// The implementation is [`Form::is_comparable`]: it names no language, and a
/// second frontend needed it, so it belongs with the form rather than here.
pub fn is_comparable(form: &Form) -> bool {
    form.is_comparable()
}

/// Whether a derived behavior is specific enough to report when matched.
///
/// The implementation is [`Form::is_reportable`], for the same reason.
pub fn is_reportable(form: &Form) -> bool {
    form.is_reportable()
}

/// The normal form of one function's body.
///
/// The laws of iteration run here rather than at the end, because what an
/// implementation *does* is only visible in normal form: `find` describes no
/// sequence operation until its fold has become a traversal, and the search for
/// delegation would give up on it before ever seeing the work.
fn normalize(function: &LibraryFunction<'_>) -> Form {
    infact_rust_normalize::normalize_function(function.node, &function.file.source).simplify()
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
    let source_of = sources(&parsed.files);
    let catalog = catalog_from(&parsed, &functions, package, version)?;
    let callables = callables(&functions, &source_of);
    let library = Library::new(
        &callables,
        type_declarations(&parsed.files, &source_of),
        CONTRACT_METHODS,
    );
    // Most callables describe no comparable behavior, which is expected and
    // skipped. A parse or source failure on the same path is not, and would
    // otherwise leave a short behavior list that reads like a small library.
    let mut behaviors = Vec::new();
    let mut skipped = std::collections::BTreeMap::new();
    for callable in &catalog.callables {
        match derive_from(&functions, &library, &catalog, &callable.path) {
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
