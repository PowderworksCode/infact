//! Facts matching Rust code behavior to external library APIs.

mod derivation;
mod idioms;
mod macro_derivation;
mod pack;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{LoadedParser, ParsedFile, ParserCatalog, parse_repository};
use heck::{ToKebabCase, ToSnakeCase};
use infact_core::{
    DERIVED_LIBRARY_BEHAVIOR_SCHEMA, DERIVED_MACRO_BEHAVIOR_SCHEMA, Derivation,
    DerivedLibraryBehavior, DerivedMacroBehavior, EXTERNAL_CATALOG_SCHEMA, ExternalCatalog, Fact,
    Form, InputEvidence, LibraryBehaviorMatch, LibraryTarget, MacroBehavior, SourceSpan,
    StringCase,
};
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

pub use derivation::{
    DerivedLibrary, derive_behavior, derive_catalog, derive_library, is_comparable, is_reportable,
};
pub use idioms::{Context, Idiom, IdiomRefusal, Recognized, all_different};
pub use macro_derivation::{MacroDerivationRequest, derive_macro_behavior};
pub use pack::{
    BuiltLibraryPack, LibraryPackRequest, behavior_file_name, build_library_pack, registry_sources,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub files_parsed: usize,
    pub matches: Vec<Fact<LibraryBehaviorMatch>>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Parser(#[from] entl_tree_sitter::Error),
    #[error(transparent)]
    Codebase(#[from] entl_codebase::Error),
    #[error("source file {path} is too large for source coordinates")]
    SourceTooLarge { path: PathBuf },
    #[error("reading the Cargo registry at {}: {source}", path.display())]
    ReadRegistry {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "external catalog for {package} {version} uses schema {actual}; supported schema is {expected}"
    )]
    UnsupportedCatalogSchema {
        package: String,
        version: String,
        actual: u32,
        expected: u32,
    },
    #[error("automatic behavior derivation is not supported for {callable}")]
    UnsupportedDerivation { callable: String },
    #[error("external catalog does not contain {callable}")]
    MissingCallable { callable: String },
    #[error("external catalog contains an incompatible signature for {callable}")]
    IncompatibleCallable { callable: String },
    #[error("could not find the source implementation of {callable}")]
    MissingImplementation { callable: String },
    #[error("cannot normalize the implementation of {callable}: {reason}")]
    UnsupportedImplementation { callable: String, reason: String },
    #[error("derived behavior for {callable} uses schema {actual}; supported schema is {expected}")]
    UnsupportedBehaviorSchema {
        callable: String,
        actual: u32,
        expected: u32,
    },
    #[error(
        "derived macro behavior for {derive} uses schema {actual}; supported schema is {expected}"
    )]
    UnsupportedMacroBehaviorSchema {
        derive: String,
        actual: u32,
        expected: u32,
    },
    #[error("no Rust parser pack is configured")]
    MissingRustParser,
    #[error("no loaded parser for pack {pack}, whose queries the match needs")]
    MissingParser { pack: String },
    #[error("{derive} produced Rust containing parse errors")]
    InvalidMacroExpansion { derive: String },
    #[error("probe for {derive} contains Rust parse errors")]
    InvalidMacroProbe { derive: String },
    #[error("cannot normalize the expansion of {derive}: {reason}")]
    UnsupportedMacroExpansion { derive: String, reason: String },
    #[error("writing pack content {}: {source}", path.display())]
    WritePack {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("building the pack manifest: {source}")]
    PackManifest {
        #[source]
        source: infact_fact_pack::ManifestError,
    },
    #[error("encoding pack content: {0}")]
    Encode(#[source] serde_json::Error),
}

impl Error {
    /// Whether this says "there is no behavior here" rather than "this broke".
    ///
    /// Most callables in a library describe nothing comparable, so a bulk
    /// derivation has to skip them. It must not skip a parse failure or an
    /// unreadable source on the same path, which is why the two are separated
    /// here instead of at each call site.
    pub const fn is_underivable(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedDerivation { .. }
                | Self::MissingCallable { .. }
                | Self::IncompatibleCallable { .. }
                | Self::MissingImplementation { .. }
                | Self::UnsupportedImplementation { .. }
                | Self::UnsupportedMacroExpansion { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Match repository code against derived library behaviors.
///
/// Every behavior is compared the same way: normalize the repository function,
/// then look for the behavior's form inside it. Nothing here knows which
/// library or which API is being matched, so a newly derived behavior becomes
/// matchable without any code changing.
pub fn analyze_repository(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalogs: &[ExternalCatalog],
    behaviors: &[DerivedLibraryBehavior],
    macro_behaviors: &[DerivedMacroBehavior],
) -> Result<AnalysisReport> {
    for catalog in catalogs {
        if catalog.schema != EXTERNAL_CATALOG_SCHEMA {
            return Err(Error::UnsupportedCatalogSchema {
                package: catalog.package.clone(),
                version: catalog.version.clone(),
                actual: catalog.schema,
                expected: EXTERNAL_CATALOG_SCHEMA,
            });
        }
    }
    for behavior in behaviors {
        if behavior.schema != DERIVED_LIBRARY_BEHAVIOR_SCHEMA {
            return Err(Error::UnsupportedBehaviorSchema {
                callable: behavior.callable_path.clone(),
                actual: behavior.schema,
                expected: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
            });
        }
    }
    for behavior in macro_behaviors {
        if behavior.schema != DERIVED_MACRO_BEHAVIOR_SCHEMA {
            return Err(Error::UnsupportedMacroBehaviorSchema {
                derive: behavior.derive_path.clone(),
                actual: behavior.schema,
                expected: DERIVED_MACRO_BEHAVIOR_SCHEMA,
            });
        }
    }

    // A behavior is only worth matching if it describes enough to be specific.
    // Below the floor, unrelated code collides: getters, one-line delegations,
    // and tuple accessors all reduce to the same handful of shapes.
    let reportable = behaviors
        .iter()
        .filter(|behavior| derivation::is_reportable(&behavior.program))
        .collect::<Vec<_>>();

    let parsed = parse_repository(root, parsers)?;
    let mut matches = BTreeSet::new();
    for file in &parsed.files {
        if file.pack.language().id != "rust" {
            continue;
        }
        for function in infact_rust_normalize::normalize_file(file) {
            // A library spells one behavior several ways: `counts` and
            // `counts_with_hasher` differ only in what the caller supplies, so
            // they derive the same form and would otherwise be reported
            // separately against the same code. Name the one a reader would
            // reach for, which is the least qualified.
            // Behaviors are compared in normal form, so the code being scanned
            // is put in the same one. Locating stays on the form as written,
            // because that is what the spans were taken from.
            let candidate = function.form.simplify();
            // An idiom is recognized directly rather than derived, because no
            // library writes the thing it replaces. Same fact, same evidence,
            // different route to the shape.
            collect_idiom_matches(file, &function, &candidate, catalogs, &mut matches)?;
            // Behaviors that share a form are indistinguishable here by
            // construction, so all of them are kept and reported together.
            let mut best: BTreeMap<&Form, (Vec<&&DerivedLibraryBehavior>, bool)> = BTreeMap::new();
            for behavior in &reportable {
                // A behavior derived from this very file is not a finding: the
                // library is not reimplementing itself. The content digest says
                // so exactly, without needing to be told which package is being
                // scanned.
                if derived_from(behavior, file) {
                    continue;
                }
                // an exact match is the stronger claim, so it is tried first
                let fused = if candidate.contains(&behavior.program) {
                    false
                } else if candidate.contains_fused(&behavior.program) {
                    true
                } else {
                    continue;
                };
                best.entry(&behavior.program)
                    .and_modify(|chosen| chosen.0.push(behavior))
                    .or_insert_with(|| (vec![behavior], fused));
            }
            // Where each matched behavior actually lands, worked out before
            // anything is reported, because which behavior to report is a
            // question about what else matched THERE.
            let mut found = Vec::new();
            for (program, (mut sharing, fused)) in best {
                // Only callables whose catalog is present can be named.
                sharing.retain(|behavior| catalog_for(catalogs, behavior).is_some());
                // Name the API a caller should reach for, which is the one
                // highest up the delegation chain. `BinaryHeap::clear` is
                // `self.drain()`, so both derive one form — but `clear` is what
                // the library offers for this and `drain` is how it is built.
                // Recommending the helper would be telling someone to reach past
                // the API that exists for exactly their case.
                sharing.sort_by(|left, right| {
                    delegates_to(right, left)
                        .cmp(&delegates_to(left, right))
                        .then_with(|| {
                            (left.callable_path.len(), &left.callable_path)
                                .cmp(&(right.callable_path.len(), &right.callable_path))
                        })
                });
                let Some((behavior, rest)) = sharing.split_first() else {
                    continue;
                };
                let alternatives: Vec<LibraryTarget> = rest
                    .iter()
                    .filter_map(|other| {
                        let catalog = catalog_for(catalogs, other)?;
                        Some(LibraryTarget::Callable {
                            package: catalog.package.clone(),
                            version: catalog.version.clone(),
                            path: other.callable_path.clone(),
                            catalog_sha256: catalog.source_sha256.clone(),
                        })
                    })
                    .collect();
                // Code that does the same thing four times has four findings.
                // Reporting only the first surfaced the rest one re-run at a
                // time, and a reader fixing the one they were shown had no way
                // to know the others existed.
                let mut located = function.form.locate_all(program);
                if located.is_empty() {
                    located = candidate.locate_all(program);
                }
                // A behavior that matched but cannot be placed is still a
                // finding; it just has no span to point at.
                let placements: Vec<Option<std::ops::Range<usize>>> = if located.is_empty() {
                    vec![None]
                } else {
                    located.into_iter().map(Some).collect()
                };
                found.push((program, *behavior, alternatives, fused, placements));
            }
            for (index, (program, behavior, alternatives, fused, placements)) in
                found.iter().enumerate()
            {
                let catalog = match catalog_for(catalogs, behavior) {
                    Some(catalog) => catalog,
                    None => continue,
                };
                for steps in placements {
                    // A behavior that another matched behavior is broader than
                    // says strictly less about this code than that one does,
                    // and saying both is saying the weaker thing twice.
                    if found.iter().enumerate().any(|(other, entry)| {
                        other != index && entry.4.contains(steps) && is_broader(program, entry.0)
                    }) {
                        continue;
                    }
                    matches.insert(behavior_match(
                        file,
                        &function,
                        steps.clone(),
                        *fused,
                        catalog,
                        behavior,
                        alternatives.clone(),
                    )?);
                }
            }
        }
        let parser = parsed
            .parsers
            .get(&file.pack.manifest().id)
            .ok_or_else(|| Error::MissingParser {
                pack: file.pack.manifest().id.clone(),
            })?;
        collect_enum_macro_matches(parser, file, macro_behaviors, &mut matches)?;
    }

    Ok(AnalysisReport {
        files_parsed: parsed.files.len(),
        matches: matches.into_iter().collect(),
        diagnostics: parsed
            .diagnostics
            .into_iter()
            .map(|diagnostic| AnalysisDiagnostic {
                path: diagnostic.path,
                message: diagnostic.message,
            })
            .collect(),
    })
}

/// Whether a behavior was derived from the file now being scanned.
///
/// Entl records a file's digest as bare hex; a derived behavior records the
/// same digest prefixed with its algorithm, so the comparison ignores that.
fn derived_from(behavior: &DerivedLibraryBehavior, file: &ParsedFile) -> bool {
    behavior.implementation.iter().any(|evidence| {
        evidence
            .source_sha256
            .rsplit(':')
            .next()
            .is_some_and(|digest| digest == file.provenance.source_sha256)
    })
}

/// Whether one callable path is the plainer way to ask for a behavior.
///
/// Shorter wins, so `counts` is preferred over `counts_with_hasher`; ties break
/// alphabetically so the choice does not depend on pack ordering.
/// Whether `outer` reaches its behavior by way of `inner`.
///
/// Derivation records the chain of functions it followed, so a wrapper's
/// evidence names the helper it delegated to. That is what makes one of two
/// identical forms the higher-level API rather than an arbitrary pick.
fn delegates_to(outer: &DerivedLibraryBehavior, inner: &DerivedLibraryBehavior) -> bool {
    let target = inner.callable_path.rsplit("::").next().unwrap_or_default();
    outer.callable_path != inner.callable_path
        && outer
            .implementation
            .iter()
            .skip(1)
            .any(|step| step.callable_path == target)
}

/// Whether one behavior's form describes everything another's does, and more.
///
/// A form used as a pattern accepts wherever it matches, so a form that matches
/// ANOTHER behavior's form accepts everywhere that one does and elsewhere
/// besides. `Option::and_then` is `match self { Some(x) => f(x), None => None }`
/// and the hole swallows what every narrower way of consuming an `Option` puts
/// there — `map`, `filter`, `ok_or`. Measured on clippy's `manual_map` test it
/// landed on fifteen of the same lines `map` did, saying less about each.
///
/// Being broader is not being wrong, and it is not grounds for leaving a
/// behavior out of a pack: code that really does reimplement `and_then` should
/// hear about it. It is only grounds for standing aside where something
/// narrower has already landed, which is why this is asked per placement rather
/// than once per pack.
fn is_broader(broad: &Form, narrow: &Form) -> bool {
    broad != narrow && narrow.contains(broad) && !broad.contains(narrow)
}

fn catalog_for<'a>(
    catalogs: &'a [ExternalCatalog],
    behavior: &DerivedLibraryBehavior,
) -> Option<&'a ExternalCatalog> {
    catalogs.iter().find(|catalog| {
        catalog.package == behavior.callable_package
            && catalog.version == behavior.callable_version
            && catalog.source_sha256 == behavior.catalog_sha256
    })
}

#[expect(
    dead_code,
    reason = "kept until type information decides between candidates"
)]
fn is_plainer(candidate: &str, current: &str) -> bool {
    (candidate.len(), candidate) < (current.len(), current)
}

/// The statements a match occupies, or the whole function when it has no run.
///
/// A behavior usually occupies a run of consecutive statements inside a larger
/// function, and naming the function alone leaves a reader to find it again.
/// When the run cannot be located — the behavior matched somewhere nested, or
/// the body is a single expression — the function is the honest answer.
fn located_span(
    file: &ParsedFile,
    function: &infact_rust_normalize::NormalizedFunction,
    located: Option<std::ops::Range<usize>>,
) -> SourceSpan {
    located
        .and_then(|steps| {
            let first = function.statements.get(steps.start)?;
            let last = function.statements.get(steps.end.checked_sub(1)?)?;
            Some(SourceSpan {
                path: file.path.clone(),
                start_byte: Some(first.start_byte),
                end_byte: Some(last.end_byte),
                start_line: first.start_line,
                end_line: last.end_line,
                start_column: None,
                end_column: None,
            })
        })
        .unwrap_or_else(|| SourceSpan {
            path: file.path.clone(),
            start_byte: Some(function.start_byte),
            end_byte: Some(function.end_byte),
            start_line: function.start_line,
            end_line: function.end_line,
            start_column: None,
            end_column: None,
        })
}

/// What was read to reach a finding, named by the analyzer that reached it.
///
/// Every fact this crate emits carries the same evidence about the same file,
/// and it was written out once per emitter until the third emitter made the
/// pattern hard to miss.
fn derivation_of(file: &ParsedFile, analyzer: &str) -> Derivation {
    Derivation {
        analyzer: analyzer.to_owned(),
        analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
        inputs: vec![InputEvidence {
            path: file.path.clone(),
            content_sha256: file.provenance.source_sha256.clone(),
            parser_id: file.provenance.parser_id.clone(),
            parser_version: file.provenance.parser_version.clone(),
            grammar_sha256: file.provenance.grammar_sha256.clone(),
            queries_sha256: file.provenance.queries_sha256.clone(),
        }],
    }
}

/// Report a match at the statements that carry it.
fn behavior_match(
    file: &ParsedFile,
    function: &infact_rust_normalize::NormalizedFunction,
    located: Option<std::ops::Range<usize>>,
    fused: bool,
    catalog: &ExternalCatalog,
    behavior: &DerivedLibraryBehavior,
    alternatives: Vec<LibraryTarget>,
) -> Result<Fact<LibraryBehaviorMatch>> {
    Ok(Fact {
        value: LibraryBehaviorMatch {
            target: LibraryTarget::Callable {
                package: catalog.package.clone(),
                version: catalog.version.clone(),
                path: behavior.callable_path.clone(),
                catalog_sha256: catalog.source_sha256.clone(),
            },
            alternatives,
            span: located_span(file, function, located),
            fused,
            // A derived behavior is the library's own implementation, so
            // matching it says the code IS what the API does. Nothing further
            // has to hold for the swap.
            conditions: Vec::new(),
        },
        derivation: derivation_of(file, "rust.library-behaviors"),
    })
}

/// Span helpers shared with derivation.
pub(crate) fn span_of(file: &ParsedFile, node: Node<'_>) -> Result<SourceSpan> {
    let too_large = || Error::SourceTooLarge {
        path: file.path.clone(),
    };
    Ok(SourceSpan {
        path: file.path.clone(),
        start_byte: Some(u64::try_from(node.start_byte()).map_err(|_| too_large())?),
        end_byte: Some(u64::try_from(node.end_byte()).map_err(|_| too_large())?),
        start_line: u32::try_from(node.start_position().row + 1).map_err(|_| too_large())?,
        end_line: u32::try_from(node.end_position().row + 1).map_err(|_| too_large())?,
        start_column: None,
        end_column: None,
    })
}

pub(crate) fn source_sha256(source: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source);
    format!("sha256:{:x}", hasher.finalize())
}

/// The enum shapes a strum derive would have produced, recognized by query.
///
/// Recognition lives in the rust parser pack's `queries/behaviors.scm`. What
/// stays here is everything a Tree-sitter query cannot state, and all of it is
/// counting: a body's arity, whether *every* arm or variant matched rather
/// than some, and whether an element sequence equals the variant list in
/// order. A query is existential; these are not.
#[derive(Default)]
struct EnumShapes<'tree> {
    /// Unit enums by their `enum_item` node, with the variants in source order.
    units: Vec<(String, Vec<String>, Node<'tree>)>,
    /// `as_str` mappings by the type the impl is on, with the impl node.
    as_strs: BTreeMap<String, (BTreeMap<String, String>, Node<'tree>)>,
    /// Types whose `Display` delegates to `as_str`, with the impl node.
    displays: BTreeMap<String, Node<'tree>>,
    /// Variant arrays by type, in element order, with the impl node.
    arrays: BTreeMap<String, (Vec<String>, Node<'tree>)>,
    /// `serde(rename_all = "..")` attributes by their `attribute_item` node.
    serde_cases: BTreeMap<usize, StringCase>,
}

fn enum_shapes<'tree>(
    parser: &'tree LoadedParser,
    file: &'tree ParsedFile,
) -> Result<EnumShapes<'tree>> {
    let matches = parser.matches("behaviors", file)?;
    let mut shapes = EnumShapes::default();

    // Unit enums. One match per variant. A variant carrying a payload has
    // extra children and simply does not match, so requiring every variant to
    // appear is what rejects a non-unit enum.
    //
    // The denominator counts `enum_variant` children only. `line_comment` is a
    // NAMED child of `enum_variant_list`, so counting every named child makes
    // any doc-commented enum fail coverage and vanish from matching entirely.
    let mut units: BTreeMap<usize, (String, Vec<String>, usize, Node<'_>)> = BTreeMap::new();
    for matched in &matches {
        let (Some(name_node), Some(variant), Some(body)) = (
            matched.capture("unit-enum.name"),
            matched.capture("unit-enum.variant"),
            matched.capture("unit-enum.body"),
        ) else {
            continue;
        };
        let (Some(name), Some(variant), Some(item)) = (
            node_text(name_node, &file.source),
            node_text(variant, &file.source),
            name_node.parent(),
        ) else {
            continue;
        };
        let entry = units.entry(item.id()).or_insert_with(|| {
            let mut cursor = body.walk();
            let total = body
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "enum_variant")
                .count();
            (name.to_owned(), Vec::new(), total, item)
        });
        entry.1.push(variant.to_owned());
    }
    for (_, (name, variants, total, item)) in units {
        if variants.len() == total && !variants.is_empty() {
            shapes.units.push((name, variants, item));
        }
    }
    shapes.units.sort_by_key(|(_, _, item)| item.start_byte());

    // Manual `as_str`. One match per arm.
    let mut as_strs: BTreeMap<usize, (String, BTreeMap<String, String>, usize, Node<'_>)> =
        BTreeMap::new();
    for matched in &matches {
        let (Some(function), Some(body), Some(ty), Some(variant), Some(value), Some(scrutinee)) = (
            matched.capture("as-str.fn"),
            matched.capture("as-str.body"),
            matched.capture("as-str.type"),
            matched.capture("as-str.variant"),
            matched.capture("as-str.value"),
            matched.capture("as-str.match"),
        ) else {
            continue;
        };
        // `only_expression`: the body holds exactly the match and nothing else.
        if body.named_child_count() != 1 {
            continue;
        }
        let (Some(ty), Some(variant), Some(value)) = (
            node_text(ty, &file.source),
            node_text(variant, &file.source),
            node_text(value, &file.source),
        ) else {
            continue;
        };
        let Some(impl_node) = function.parent().and_then(|list| list.parent()) else {
            continue;
        };
        let arms = scrutinee
            .child_by_field_name("body")
            .map_or(0, |block| block.named_child_count());
        let entry = as_strs
            .entry(function.id())
            .or_insert_with(|| (ty.to_owned(), BTreeMap::new(), arms, impl_node));
        entry.1.insert(variant.to_owned(), value.to_owned());
    }
    for (_, (ty, mappings, arms, impl_node)) in as_strs {
        // Every arm must have matched. A partial mapping would be a confident
        // wrong answer, which is the worst thing this analyzer can produce.
        if mappings.len() == arms {
            shapes.as_strs.insert(ty, (mappings, impl_node));
        }
    }

    // `Display` delegating to `as_str`.
    for matched in &matches {
        let (Some(ty), Some(arguments), Some(body)) = (
            matched.capture("display.type"),
            matched.capture("display.inner-args"),
            matched.capture("display.body"),
        ) else {
            continue;
        };
        // `as_str()` takes no arguments, and `fmt`'s body is exactly the call.
        if arguments.named_child_count() != 0 || body.named_child_count() != 1 {
            continue;
        }
        let (Some(ty), Some(impl_node)) = (
            node_text(ty, &file.source),
            body.parent()
                .and_then(|f| f.parent())
                .and_then(|l| l.parent()),
        ) else {
            continue;
        };
        shapes.displays.insert(ty.to_owned(), impl_node);
    }

    // Manual variant arrays. One match per element; order is preserved because
    // matches arrive in source order and the caller compares sequences.
    for matched in &matches {
        let (Some(ty), Some(element), Some(item)) = (
            matched.capture("variant-array.type"),
            matched.capture("variant-array.element"),
            matched.capture("variant-array.item"),
        ) else {
            continue;
        };
        let (Some(ty), Some(element), Some(impl_node)) = (
            node_text(ty, &file.source),
            node_text(element, &file.source),
            item.parent().and_then(|list| list.parent()),
        ) else {
            continue;
        };
        let entry = shapes
            .arrays
            .entry(ty.to_owned())
            .or_insert_with(|| (Vec::new(), impl_node));
        entry.0.push(element.to_owned());
    }

    // `serde(rename_all = "..")`, kept by attribute node so the caller can
    // associate it the way `prev_named_sibling` always did.
    for matched in &matches {
        let Some(value) = matched.capture("serde-case.value") else {
            continue;
        };
        let case = match node_text(value, &file.source) {
            Some("kebab-case") => StringCase::Kebab,
            Some("snake_case") => StringCase::Snake,
            _ => continue,
        };
        let Some(item) = value
            .parent()
            .and_then(|n| n.parent())
            .and_then(|n| n.parent())
        else {
            continue;
        };
        shapes.serde_cases.insert(item.id(), case);
    }

    Ok(shapes)
}

/// The enum's own `serde(rename_all)`, read from the attributes above it.
fn preferred_case(item: Node<'_>, cases: &BTreeMap<usize, StringCase>) -> Option<StringCase> {
    let mut sibling = item.prev_named_sibling();
    while let Some(attribute) = sibling {
        if attribute.kind() != "attribute_item" {
            break;
        }
        if let Some(case) = cases.get(&attribute.id()) {
            return Some(*case);
        }
        sibling = attribute.prev_named_sibling();
    }
    None
}

fn collect_enum_macro_matches(
    parser: &LoadedParser,
    file: &ParsedFile,
    macro_behaviors: &[DerivedMacroBehavior],
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let display_behaviors = macro_behaviors
        .iter()
        .filter(|behavior| {
            behavior.macro_package == "strum"
                && behavior.derive_path == "strum::Display"
                && matches!(behavior.behavior, MacroBehavior::EnumDisplay { .. })
        })
        .collect::<Vec<_>>();
    let as_ref_behaviors = macro_behaviors
        .iter()
        .filter(|behavior| {
            behavior.macro_package == "strum"
                && behavior.derive_path == "strum::AsRefStr"
                && matches!(behavior.behavior, MacroBehavior::EnumAsRefStr { .. })
        })
        .collect::<Vec<_>>();
    let variant_array_behaviors = macro_behaviors
        .iter()
        .filter(|behavior| {
            behavior.macro_package == "strum"
                && behavior.derive_path == "strum::VariantArray"
                && behavior.behavior == MacroBehavior::EnumVariantArray
        })
        .collect::<Vec<_>>();
    if display_behaviors.is_empty()
        && as_ref_behaviors.is_empty()
        && variant_array_behaviors.is_empty()
    {
        return Ok(());
    }

    let shapes = enum_shapes(parser, file)?;
    for (name, variants, item) in &shapes.units {
        let inherent = shapes.as_strs.get(name);
        let exhaustive =
            inherent.is_some_and(|(mappings, _)| mapping_is_exhaustive(mappings, variants));

        if let Some((mappings, inherent_impl)) = inherent
            && exhaustive
        {
            if let Some(display_impl) = shapes.displays.get(name) {
                for behavior in &display_behaviors {
                    let MacroBehavior::EnumDisplay { case } = behavior.behavior else {
                        continue;
                    };
                    if mappings_match_case(mappings, case) {
                        output.insert(macro_behavior_match(file, *item, *display_impl, behavior)?);
                    }
                }
            }

            let preferred = preferred_case(*item, &shapes.serde_cases);
            let matching = as_ref_behaviors
                .iter()
                .copied()
                .filter(|behavior| {
                    let MacroBehavior::EnumAsRefStr { case } = behavior.behavior else {
                        return false;
                    };
                    preferred.is_none_or(|want| want == case) && mappings_match_case(mappings, case)
                })
                .collect::<Vec<_>>();
            // Naming a case the code does not decide would be inventing
            // certainty: single-word variants read the same in every case.
            if preferred.is_some() || matching.len() == 1 {
                for behavior in matching {
                    output.insert(macro_behavior_match(file, *item, *inherent_impl, behavior)?);
                }
            }
        }

        if let Some((elements, array_impl)) = shapes.arrays.get(name)
            && elements == variants
        {
            for behavior in &variant_array_behaviors {
                output.insert(macro_behavior_match(file, *item, *array_impl, behavior)?);
            }
        }
    }
    Ok(())
}

/// Report the algorithms recognized in one normalized function.
///
/// An idiom names the API it recommends by path, so it can only be reported
/// when a catalog for that package is loaded — the same rule derived behaviors
/// follow, and for the same reason: naming a version that was never read would
/// be inventing provenance.
///
/// What separates this from the derived-behavior loop above is only where the
/// shape comes from: there it is normalized out of a library's own source, here
/// it is written down, because no library writes the thing it replaces.
/// Everything after the shape — matching it, placing it, naming the target,
/// carrying the evidence — is the same code.
fn collect_idiom_matches(
    file: &ParsedFile,
    function: &infact_rust_normalize::NormalizedFunction,
    candidate: &Form,
    catalogs: &[ExternalCatalog],
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let context = idioms::Context {
        can_allocate: !function.is_const,
    };
    for idiom in idioms::Idiom::ALL {
        collect_one_idiom(*idiom, file, function, candidate, context, catalogs, output)?;
    }
    Ok(())
}

fn collect_one_idiom(
    idiom: idioms::Idiom,
    file: &ParsedFile,
    function: &infact_rust_normalize::NormalizedFunction,
    candidate: &Form,
    context: idioms::Context,
    catalogs: &[ExternalCatalog],
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let Ok(walked) = idioms::recognize(idiom, candidate, context) else {
        return Ok(());
    };
    let (package, path) = idiom.callable_path();
    // The callable has to be present AND still answer the question the idiom
    // decides. A catalog is generated data and a path is not a promise: the
    // signature is what says the API still does this.
    let found = catalogs.iter().find_map(|catalog| {
        let callable = catalog
            .callables
            .iter()
            .find(|callable| callable.path == path)?;
        (catalog.package == package && idioms::answers_a_predicate(callable))
            .then_some((catalog, callable))
    });
    let Some((catalog, callable)) = found else {
        return Ok(());
    };
    // Point at the statements the walk occupies rather than the whole function,
    // by the route every other finding is placed by. The shape that matched is
    // rebuilt from what the recognizer resolved, so what gets located is what
    // was actually recognized.
    let mut located = function.form.locate_all(&walked.shape);
    if located.is_empty() {
        located = candidate.locate_all(&walked.shape);
    }
    let placements = if located.is_empty() {
        vec![None]
    } else {
        located.into_iter().map(Some).collect()
    };
    for steps in placements {
        output.insert(Fact {
            value: LibraryBehaviorMatch {
                target: LibraryTarget::Callable {
                    package: catalog.package.clone(),
                    version: catalog.version.clone(),
                    path: path.to_owned(),
                    catalog_sha256: catalog.source_sha256.clone(),
                },
                // The recognizer names one API, so there is nothing to choose
                // between; what is uncertain about the recommendation is in the
                // conditions rather than in which callable it points at.
                alternatives: Vec::new(),
                fused: false,
                span: located_span(file, function, steps),
                conditions: idiom.conditions(callable),
            },
            derivation: derivation_of(file, "rust.idioms"),
        });
    }
    Ok(())
}

fn mapping_is_exhaustive(mappings: &BTreeMap<String, String>, variants: &[String]) -> bool {
    mappings.keys().cloned().collect::<BTreeSet<_>>()
        == variants.iter().cloned().collect::<BTreeSet<_>>()
}

fn mappings_match_case(mappings: &BTreeMap<String, String>, case: StringCase) -> bool {
    mappings.iter().all(|(variant, value)| match case {
        StringCase::Kebab => variant.to_kebab_case() == *value,
        StringCase::Snake => variant.to_snake_case() == *value,
    })
}

fn macro_behavior_match(
    file: &ParsedFile,
    first: Node<'_>,
    second: Node<'_>,
    behavior: &DerivedMacroBehavior,
) -> Result<Fact<LibraryBehaviorMatch>> {
    let (start, end) = if first.start_byte() <= second.start_byte() {
        (first, second)
    } else {
        (second, first)
    };
    let start_byte = u64::try_from(start.start_byte()).map_err(|_| Error::SourceTooLarge {
        path: file.path.clone(),
    })?;
    let end_byte = u64::try_from(end.end_byte()).map_err(|_| Error::SourceTooLarge {
        path: file.path.clone(),
    })?;
    let start_line =
        u32::try_from(start.start_position().row + 1).map_err(|_| Error::SourceTooLarge {
            path: file.path.clone(),
        })?;
    let end_line =
        u32::try_from(end.end_position().row + 1).map_err(|_| Error::SourceTooLarge {
            path: file.path.clone(),
        })?;
    Ok(Fact {
        value: LibraryBehaviorMatch {
            target: LibraryTarget::DeriveMacro {
                package: behavior.macro_package.clone(),
                version: behavior.macro_version.clone(),
                path: behavior.derive_path.clone(),
                expansion_sha256: behavior.expansion_sha256.clone(),
            },
            // a derive names exactly one macro
            alternatives: Vec::new(),
            fused: false,
            conditions: Vec::new(),
            span: SourceSpan {
                path: file.path.clone(),
                start_byte: Some(start_byte),
                end_byte: Some(end_byte),
                start_line,
                end_line,
                start_column: None,
                end_column: None,
            },
        },
        derivation: Derivation {
            analyzer: "rust.library-behaviors".to_owned(),
            analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
            inputs: vec![InputEvidence {
                path: file.path.clone(),
                content_sha256: file.provenance.source_sha256.clone(),
                parser_id: file.provenance.parser_id.clone(),
                parser_version: file.provenance.parser_version.clone(),
                grammar_sha256: file.provenance.grammar_sha256.clone(),
                queries_sha256: file.provenance.queries_sha256.clone(),
            }],
        },
    })
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}
