//! Reading a JavaScript or TypeScript library into callables and behaviors.
//!
//! Everything here is about how ECMAScript spells a function, a type, and a
//! public name. The walk from a callable to the body that does its work is not
//! here at all — that is [`infact_behaviors`], which Rust uses unchanged.
//!
//! The standard library is the reason this crate exists and also the reason it
//! looks unusual. TypeScript's `lib.es5.d.ts` holds no implementations: every
//! builtin is a declaration and nothing else, so there is nothing to derive
//! from. SpiderMonkey self-hosts 89 of them in plain JavaScript, and that is
//! what a language pack is derived from — read locally, exactly as the Rust std
//! pack is read from the local rustup toolchain.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use entl_codebase::{InventoryOptions, walk};
use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime};
use infact_behaviors::{Library, LibraryCallable, Refusal, SourceId, container_name, leaf_name};
use infact_core::{
    CallableContainer, DerivedLibraryBehavior, EXTERNAL_CATALOG_SCHEMA, ExternalCallable,
    ExternalCatalog, ImplementationEvidence,
};
use tree_sitter::Node;

use crate::{DERIVED_LIBRARY_BEHAVIOR_SCHEMA, Result, source_sha256, span_of};

/// The methods this language requires of a type that can be followed into.
///
/// `next` is the iterator protocol, which is the whole of what JavaScript
/// demands of a type that produces a sequence. Rust's list is longer because
/// Rust has more such protocols; that difference is exactly why the walk takes
/// the list rather than holding one.
const CONTRACT_METHODS: &[&str] = &["next"];

/// Whether a parser pack reads a dialect this crate normalizes.
///
/// One normalizer serves all three: `tree-sitter-typescript` extends
/// `tree-sitter-javascript`, and TSX only adds element syntax. This matters
/// because a library implementation being derived from is JavaScript while the
/// code being analyzed is usually TypeScript.
pub(crate) fn is_ecmascript(file: &ParsedFile) -> bool {
    let language = file.pack.language();
    language.id == "typescript" || language.id == "javascript" || language.id == "tsx"
}

/// A library's sources, read whole where possible and in part where not.
pub(crate) struct ParsedLibrary {
    pub(crate) files: Vec<ParsedFile>,
    /// Files that parsed with errors and were read anyway, with the first one.
    pub(crate) damaged: Vec<String>,
    /// Files that could not be read at all.
    pub(crate) unreadable: Vec<String>,
}

/// Parse a library, keeping what a file with an error in it still says.
///
/// `parse_repository` discards any file whose tree has an error, which is right
/// for a repository being analyzed: the answer there is about the file. It is
/// wrong for a library being derived FROM. An engine's self-hosted builtins are
/// preprocessed, so `Array.js` carries three `#if` lines the JavaScript grammar
/// cannot read — and discarding the file for them costs all thirty functions in
/// it. Measured: 89 top-level functions across the four files, of which
/// discarding whole files left 18.
///
/// Tree-sitter recovers locally, so the damage is bounded to the construct that
/// caused it. Every function whose own subtree carries an error is declined by
/// name below; the rest are exactly as readable as they were.
///
/// The alternative, a dialect rewrite that blanked the directives, is worse and
/// was considered: it would leave BOTH branches of the conditional in place and
/// silently merge them into one body.
fn parse_library(root: impl AsRef<Path>, catalog: &ParserCatalog) -> Result<ParsedLibrary> {
    let tree = walk(root, &InventoryOptions::default())?;
    let runtime = ParserRuntime::new()?;
    let mut parsed = ParsedLibrary {
        files: Vec::new(),
        damaged: Vec::new(),
        unreadable: tree
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.path.display(), diagnostic.message))
            .collect(),
    };
    for file in &tree.files {
        let Some(language) = file
            .language
            .as_ref()
            .map(|detection| detection.language.as_str())
        else {
            continue;
        };
        let Some(pack) = catalog.resolve(language, &file.path) else {
            continue;
        };
        let source = match tree.read_bytes(&file.path) {
            Ok(source) => source,
            Err(error) => {
                parsed
                    .unreadable
                    .push(format!("{}: {error}", file.path.display()));
                continue;
            }
        };
        let read = runtime
            .load(pack.clone())?
            .parse(file.path.clone(), std::sync::Arc::<[u8]>::from(source))?;
        if read.tree.root_node().has_error() {
            parsed.damaged.push(file.path.display().to_string());
        }
        parsed.files.push(read);
    }
    Ok(parsed)
}

/// A function found in the library source.
pub(crate) struct LibraryFunction<'a> {
    pub(crate) file: &'a ParsedFile,
    /// The node a reader would point at, which is the whole declaration.
    pub(crate) node: Node<'a>,
    pub(crate) name: String,
    /// The class the function is written inside, when there is one.
    pub(crate) container: Option<String>,
    /// Whether the module makes this name reachable from outside the file.
    pub(crate) exported: bool,
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}

/// The definition a name is bound to, which is not always the node named.
///
/// `const f = () => {}` names the binding on the declarator and holds the body
/// on the arrow. Normalizing the declarator would find no parameters and no
/// body, which reads downstream as a function that does nothing.
fn definition<'a>(node: Node<'a>) -> Option<Node<'a>> {
    match node.kind() {
        "variable_declarator" => node.child_by_field_name("value"),
        _ => Some(node),
    }
}

/// Whether this node is a function bound to a name.
///
/// TypeScript writes a quarter of its named functions as `const f = () => ..`,
/// so collecting only declarations and methods leaves them unread — and unread
/// reads downstream as "nothing there".
fn named_function<'a>(node: Node<'a>) -> Option<(Node<'a>, Node<'a>)> {
    let name = node.child_by_field_name("name")?;
    match node.kind() {
        "function_declaration" | "method_definition" => Some((name, node)),
        "variable_declarator"
            if name.kind() == "identifier"
                && node.child_by_field_name("value").is_some_and(|value| {
                    matches!(value.kind(), "arrow_function" | "function_expression")
                }) =>
        {
            Some((name, node))
        }
        _ => None,
    }
}

fn collect_functions<'a>(
    node: Node<'a>,
    file: &'a ParsedFile,
    container: Option<&str>,
    exported: bool,
    output: &mut Vec<LibraryFunction<'a>>,
    damaged: &mut Vec<String>,
) {
    if let Some((name, declaration)) = named_function(node)
        && let Some(name) = node_text(name, &file.source)
    {
        // A function the parser could not read whole is declined by name.
        // Its body is missing something, and normalizing what survived would
        // describe a behavior nobody wrote — a confident wrong answer, which
        // is the worst thing this can produce. It is not collected at all, so
        // it is neither a callable to derive nor a delegation to follow into.
        if declaration.has_error() {
            damaged.push(name.to_owned());
        } else {
            output.push(LibraryFunction {
                file,
                node: declaration,
                name: name.to_owned(),
                container: container.map(str::to_owned),
                exported,
            });
        }
    }
    // a class qualifies every method written inside it, exactly as an `impl`
    // does in Rust
    let nested = match node.kind() {
        "class_declaration" | "class" | "abstract_class_declaration" => node
            .child_by_field_name("name")
            .and_then(|name| node_text(name, &file.source)),
        _ => container,
    };
    // `export class Foo { .. }` exports every method it declares, and
    // `export const f = () => ..` exports the binding underneath it
    let nested_exported = exported || node.kind() == "export_statement";
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_functions(child, file, nested, nested_exported, output, damaged);
    }
}

/// Whether a path is the library's own source rather than its tests or its
/// build output.
///
/// These directory and suffix conventions are the ecosystem's, not any one
/// package's, which is why they can be stated here. It matters because test
/// suites routinely define functions named after the API they exercise, and
/// because a `dist/` bundle holds a second copy of everything.
fn is_library_source(path: &Path) -> bool {
    let excluded_directory = path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(
                "node_modules"
                    | "test"
                    | "tests"
                    | "__tests__"
                    | "__mocks__"
                    | "spec"
                    | "dist"
                    | "build"
                    | "coverage"
            )
        )
    });
    let is_test_file = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.contains(".test.") || name.contains(".spec.") || name.ends_with(".d.ts")
        });
    !excluded_directory && !is_test_file
}

pub(crate) fn library_functions(files: &[ParsedFile]) -> (Vec<LibraryFunction<'_>>, Vec<String>) {
    let mut functions = Vec::new();
    let mut damaged = Vec::new();
    for file in files {
        if !is_ecmascript(file) || !is_library_source(&file.path) {
            continue;
        }
        collect_functions(
            file.tree.root_node(),
            file,
            None,
            false,
            &mut functions,
            &mut damaged,
        );
    }
    (functions, damaged)
}

/// Whether a file is a module, which is what decides what `export` means.
///
/// A file with no import and no export is a SCRIPT: its top-level declarations
/// are reachable, and demanding an `export` keyword would find nothing public in
/// it at all. That is not a corner case — every self-hosted engine builtin is
/// written that way, so a rule that ignored it would derive an empty standard
/// library while reporting success.
fn is_module(file: &ParsedFile) -> bool {
    fn walk(node: Node<'_>) -> bool {
        if matches!(node.kind(), "export_statement" | "import_statement") {
            return true;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor).any(walk)
    }
    walk(file.tree.root_node())
}

/// Names a module exports by listing them rather than by keyword.
///
/// `export { find, findLast }` at the bottom of a file is how a great deal of
/// JavaScript is written, and it puts the export nowhere near the declaration.
/// Reading only the keyword form would call those functions private and derive
/// nothing from them.
fn exported_names(file: &ParsedFile) -> BTreeSet<String> {
    fn walk(node: Node<'_>, source: &[u8], output: &mut BTreeSet<String>) {
        if node.kind() == "export_specifier"
            && let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| node_text(name, source))
        {
            output.insert(name.to_owned());
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk(child, source, output);
        }
    }
    let mut names = BTreeSet::new();
    walk(file.tree.root_node(), &file.source, &mut names);
    names
}

/// What a file makes reachable from outside it.
struct Surface {
    /// Files that are modules, where `export` decides what is public.
    modules: BTreeSet<std::path::PathBuf>,
    /// Names each module exports by listing them.
    listed: BTreeMap<std::path::PathBuf, BTreeSet<String>>,
}

impl Surface {
    fn read(files: &[ParsedFile]) -> Self {
        let mut modules = BTreeSet::new();
        let mut listed = BTreeMap::new();
        for file in files {
            if !is_ecmascript(file) || !is_module(file) {
                continue;
            }
            modules.insert(file.path.clone());
            listed.insert(file.path.clone(), exported_names(file));
        }
        Self { modules, listed }
    }

    /// Whether a function is part of the library's public surface.
    fn covers(&self, function: &LibraryFunction<'_>) -> bool {
        if !self.modules.contains(&function.file.path) {
            return true;
        }
        function.exported
            || self
                .listed
                .get(&function.file.path)
                .is_some_and(|names| names.contains(&function.name))
    }
}

/// Which sources declare each named type.
///
/// A bare type name only identifies a type when the library gives it to one, and
/// the walk refuses to follow a name two types answer to.
fn type_declarations(
    files: &[ParsedFile],
    source_of: &BTreeMap<std::path::PathBuf, SourceId>,
) -> BTreeMap<String, BTreeSet<SourceId>> {
    let mut declarations: BTreeMap<String, BTreeSet<SourceId>> = BTreeMap::new();
    for file in files {
        if !is_ecmascript(file) || !is_library_source(&file.path) {
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
    output: &mut BTreeMap<String, BTreeSet<SourceId>>,
) {
    if matches!(
        node.kind(),
        "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
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

/// The normalized bodies the walk reads, in the order the functions were found.
///
/// Every function is normalized once, up front. The walk asks for a form
/// repeatedly — ranking a type's methods reads all of them — and normalizing on
/// demand meant doing the same work once per question.
fn callables(
    functions: &[LibraryFunction<'_>],
    source_of: &BTreeMap<std::path::PathBuf, SourceId>,
) -> Vec<LibraryCallable> {
    functions
        .iter()
        .map(|function| LibraryCallable {
            name: function.name.clone(),
            container: function.container.clone(),
            source: source_of.get(&function.file.path).copied().unwrap_or(0),
            // A declaration with a name and no body is a signature. It says
            // what may be called, not what happens, and must not be confused
            // with a function that could not be found.
            form: definition(function.node)
                .filter(|definition| definition.child_by_field_name("body").is_some())
                .map(|definition| {
                    infact_ts_normalize::normalize_function(definition, &function.file.source)
                        .simplify()
                }),
        })
        .collect()
}

fn evidence(function: &LibraryFunction<'_>) -> Result<ImplementationEvidence> {
    Ok(ImplementationEvidence {
        callable_path: function.name.clone(),
        span: span_of(function.file, function.node)?,
        source_sha256: source_sha256(&function.file.source),
    })
}

/// Everything derived from one library, and what could not be read.
pub struct DerivedLibrary {
    pub catalog: ExternalCatalog,
    pub behaviors: Vec<DerivedLibraryBehavior>,
    /// Files the parser could not read.
    ///
    /// A grammar rejects a whole file when any part of it is beyond what it
    /// knows, so an unsupported construct silently removes everything beside
    /// it. Reporting these keeps "this library has no behaviors" apart from
    /// "this library could not be read", which look identical otherwise.
    pub unparsed: Vec<String>,
    /// Functions declined because the parser could not read them whole.
    ///
    /// Named rather than counted, because which ones they are is the question a
    /// reader has: one damaged helper nobody reimplements costs nothing, and one
    /// damaged `find` costs the whole point of the pack.
    pub damaged: Vec<String>,
    /// Why the callables that produced nothing produced nothing, counted.
    pub skipped: BTreeMap<String, usize>,
}

/// Derive a library's whole catalog and every behavior it yields, parsing once.
pub fn derive_library(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    package: &str,
    version: &str,
) -> Result<DerivedLibrary> {
    let parsed = parse_library(source_root, parsers)?;
    let (functions, damaged) = library_functions(&parsed.files);
    let source_of = parsed
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.path.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let surface = Surface::read(&parsed.files);
    let catalog = catalog_from(&parsed, &functions, &surface, package, version)?;

    let callables = callables(&functions, &source_of);
    let library = Library::new(
        &callables,
        type_declarations(&parsed.files, &source_of),
        CONTRACT_METHODS,
    );

    let mut behaviors = Vec::new();
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for callable in &catalog.callables {
        match library.derive(leaf_name(&callable.path), container_name(&callable.path)) {
            Ok(derived) => behaviors.push(DerivedLibraryBehavior {
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
            }),
            Err(refusal) => *skipped.entry(skip_reason(&refusal)).or_insert(0) += 1,
        }
    }

    // A function the parser could not read whole is a skip like any other, and
    // has to be counted as one. Left out of the tally, a grammar that stopped
    // reading a construct would look like a library that simply had less in it.
    if !damaged.is_empty() {
        *skipped
            .entry("the implementation could not be read whole".to_owned())
            .or_insert(0) += damaged.len();
    }
    let mut unparsed = parsed.unreadable;
    unparsed.extend(
        parsed
            .damaged
            .iter()
            .map(|path| format!("{path}: read in part, the rest of it kept")),
    );
    unparsed.sort();
    unparsed.dedup();
    Ok(DerivedLibrary {
        catalog,
        behaviors,
        unparsed,
        damaged,
        skipped,
    })
}

/// The reason a callable yielded nothing, with the callable's own name removed.
///
/// The name is what makes each message unique, and counting unique messages says
/// nothing. What is wanted is the handful of reasons behind thousands of skips.
fn skip_reason(refusal: &Refusal) -> String {
    match refusal {
        Refusal::NoImplementation => "no implementation was found".to_owned(),
        Refusal::NoBody => "the implementation has no body".to_owned(),
        Refusal::NotComparable => {
            "the implementation describes nothing that can be compared".to_owned()
        }
        Refusal::TooDeep(_) => "the implementation describes a subsystem".to_owned(),
    }
}

fn catalog_from(
    parsed: &ParsedLibrary,
    functions: &[LibraryFunction<'_>],
    surface: &Surface,
    package: &str,
    version: &str,
) -> Result<ExternalCatalog> {
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    let mut sources = parsed
        .files
        .iter()
        .filter(|file| is_ecmascript(file) && is_library_source(&file.path))
        .map(|file| (file.path.clone(), file.provenance.source_sha256.clone()))
        .collect::<Vec<_>>();
    sources.sort();
    for (path, content) in &sources {
        digest.update(path.to_string_lossy().as_bytes());
        digest.update(content.as_bytes());
    }
    let source_sha256 = format!("sha256:{:x}", digest.finalize());

    let mut callables = functions
        .iter()
        .filter(|function| surface.covers(function))
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
        // zero records that no generated documentation was involved
        rustdoc_format: 0,
        source_sha256,
        callables,
    })
}
