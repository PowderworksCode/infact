//! Facts matching Rust code behavior to external library APIs.

mod derivation;
mod macro_derivation;
mod pack;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
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
pub use macro_derivation::{MacroDerivationRequest, derive_macro_behavior};
pub use pack::{
    BuiltLibraryPack, LibraryPackRequest, behavior_file_name, build_library_pack,
    registry_sources,
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
            for (mut sharing, fused) in best.into_values() {
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
                let Some(catalog) = catalog_for(catalogs, behavior) else {
                    continue;
                };
                let alternatives = rest
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
                let located = function
                    .form
                    .locate(&behavior.program)
                    .or_else(|| candidate.locate(&behavior.program));
                matches.insert(behavior_match(
                    file,
                    &function,
                    located,
                    fused,
                    catalog,
                    behavior,
                    alternatives,
                )?);
            }
        }
        collect_enum_macro_matches(file, macro_behaviors, &mut matches)?;
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

#[expect(dead_code, reason = "kept until type information decides between candidates")]
fn is_plainer(candidate: &str, current: &str) -> bool {
    (candidate.len(), candidate) < (current.len(), current)
}

/// Report a match at the statements that carry it.
///
/// A behavior usually occupies a run of consecutive statements inside a larger
/// function, and naming the function alone leaves a reader to find it again.
/// When the run cannot be located — the behavior matched somewhere nested, or
/// the body is a single expression — the function is the honest answer.
fn behavior_match(
    file: &ParsedFile,
    function: &infact_rust_normalize::NormalizedFunction,
    located: Option<std::ops::Range<usize>>,
    fused: bool,
    catalog: &ExternalCatalog,
    behavior: &DerivedLibraryBehavior,
    alternatives: Vec<LibraryTarget>,
) -> Result<Fact<LibraryBehaviorMatch>> {
    let span = located
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
        });
    Ok(Fact {
        value: LibraryBehaviorMatch {
            target: LibraryTarget::Callable {
                package: catalog.package.clone(),
                version: catalog.version.clone(),
                path: behavior.callable_path.clone(),
                catalog_sha256: catalog.source_sha256.clone(),
            },
            alternatives,
            span,
            fused,
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

fn collect_enum_macro_matches(
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

    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_item"
            && let Some((enum_name, variants)) = unit_enum(node, &file.source)
            && let Some(scope) = node.parent()
        {
            let mut cursor = scope.walk();
            let siblings = scope.named_children(&mut cursor).collect::<Vec<_>>();
            let inherent = siblings.iter().copied().find_map(|item| {
                (is_impl_for(item, None, &enum_name, &file.source))
                    .then(|| manual_as_str(item, &file.source))
                    .flatten()
                    .map(|mappings| (item, mappings))
            });
            let display = siblings.iter().copied().find(|item| {
                is_impl_for(*item, Some("Display"), &enum_name, &file.source)
                    && display_delegates_to_as_str(*item, &file.source)
            });
            if let (Some((_inherent, mappings)), Some(display)) = (&inherent, display)
                && mapping_is_exhaustive(mappings, &variants)
            {
                for behavior in &display_behaviors {
                    let MacroBehavior::EnumDisplay { case } = behavior.behavior else {
                        continue;
                    };
                    if mappings_match_case(mappings, case) {
                        output.insert(macro_behavior_match(file, node, display, behavior)?);
                    }
                }
            }
            if let Some((inherent, mappings)) = &inherent
                && mapping_is_exhaustive(mappings, &variants)
            {
                let preferred_case = enum_serde_case(node, &file.source);
                let matching = as_ref_behaviors
                    .iter()
                    .copied()
                    .filter(|behavior| {
                        let MacroBehavior::EnumAsRefStr { case } = behavior.behavior else {
                            return false;
                        };
                        preferred_case.is_none_or(|preferred| preferred == case)
                            && mappings_match_case(mappings, case)
                    })
                    .collect::<Vec<_>>();
                if preferred_case.is_some() || matching.len() == 1 {
                    for behavior in matching {
                        output.insert(macro_behavior_match(file, node, *inherent, behavior)?);
                    }
                }
            }
            if let Some(array_impl) = siblings.iter().copied().find(|item| {
                is_impl_for(*item, None, &enum_name, &file.source)
                    && manual_variant_array(*item, &variants, &file.source)
            }) {
                for behavior in &variant_array_behaviors {
                    output.insert(macro_behavior_match(file, node, array_impl, behavior)?);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn enum_serde_case(node: Node<'_>, source: &[u8]) -> Option<StringCase> {
    let mut sibling = node.prev_named_sibling();
    while let Some(attribute) = sibling {
        if attribute.kind() != "attribute_item" {
            break;
        }
        let text = node_text(attribute, source)?;
        if text.contains("serde") && text.contains("rename_all") {
            if text.contains("kebab-case") {
                return Some(StringCase::Kebab);
            }
            if text.contains("snake_case") {
                return Some(StringCase::Snake);
            }
        }
        sibling = attribute.prev_named_sibling();
    }
    None
}

fn unit_enum(node: Node<'_>, source: &[u8]) -> Option<(String, Vec<String>)> {
    let name = field_text(node, "name", source)?.to_owned();
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let variants = body
        .named_children(&mut cursor)
        .map(|variant| {
            (variant.kind() == "enum_variant" && variant.named_child_count() == 1)
                .then(|| field_text(variant, "name", source).map(str::to_owned))
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    (!variants.is_empty()).then_some((name, variants))
}

fn mapping_is_exhaustive(mappings: &BTreeMap<String, String>, variants: &[String]) -> bool {
    mappings.keys().cloned().collect::<BTreeSet<_>>()
        == variants.iter().cloned().collect::<BTreeSet<_>>()
}

fn manual_variant_array(node: Node<'_>, variants: &[String], source: &[u8]) -> bool {
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor).any(|item| {
        if item.kind() != "const_item" {
            return false;
        }
        let Some(value) = item.child_by_field_name("value") else {
            return false;
        };
        let Some(array) = named_descendant(value, "array_expression") else {
            return false;
        };
        let mut cursor = array.walk();
        array
            .named_children(&mut cursor)
            .filter_map(|element| last_named_identifier(element, source).map(str::to_owned))
            .collect::<Vec<_>>()
            == variants
    })
}

fn named_descendant<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn is_impl_for(
    node: Node<'_>,
    expected_trait: Option<&str>,
    expected_type: &str,
    source: &[u8],
) -> bool {
    if node.kind() != "impl_item" || field_text(node, "type", source) != Some(expected_type) {
        return false;
    }
    match (node.child_by_field_name("trait"), expected_trait) {
        (None, None) => true,
        (Some(trait_node), Some(expected)) => {
            last_named_identifier(trait_node, source) == Some(expected)
        }
        _ => false,
    }
}

fn last_named_identifier<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    if matches!(node.kind(), "identifier" | "type_identifier") {
        return node_text(node, source);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|child| last_named_identifier(child, source))
        .last()
}

fn manual_as_str(node: Node<'_>, source: &[u8]) -> Option<BTreeMap<String, String>> {
    let function = impl_function(node, "as_str", source)?;
    let body = function.child_by_field_name("body")?;
    let match_expression = only_expression(body)?;
    if match_expression.kind() != "match_expression"
        || match_expression
            .child_by_field_name("value")
            .is_none_or(|value| value.kind() != "self")
    {
        return None;
    }
    let match_body = match_expression.child_by_field_name("body")?;
    let mut cursor = match_body.walk();
    match_body
        .named_children(&mut cursor)
        .map(|arm| {
            let pattern = arm.child_by_field_name("pattern")?;
            let value = arm.child_by_field_name("value")?;
            if arm.kind() != "match_arm" || value.kind() != "string_literal" {
                return None;
            }
            let variant = last_named_identifier(pattern, source)?.to_owned();
            let string = value.named_child(0).and_then(|content| {
                (content.kind() == "string_content")
                    .then(|| node_text(content, source))
                    .flatten()
            })?;
            Some((variant, string.to_owned()))
        })
        .collect()
}

fn impl_function<'tree>(node: Node<'tree>, expected: &str, source: &[u8]) -> Option<Node<'tree>> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    body.named_children(&mut cursor).find(|item| {
        item.kind() == "function_item" && field_text(*item, "name", source) == Some(expected)
    })
}

fn only_expression(block: Node<'_>) -> Option<Node<'_>> {
    if block.named_child_count() != 1 {
        return None;
    }
    let expression = block.named_child(0)?;
    (expression.kind() == "expression_statement")
        .then(|| expression.named_child(0))
        .flatten()
        .or(Some(expression))
}

fn display_delegates_to_as_str(node: Node<'_>, source: &[u8]) -> bool {
    let Some(function) = impl_function(node, "fmt", source) else {
        return false;
    };
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    let Some(call) = only_expression(body) else {
        return false;
    };
    let Some(fmt_field) = call.child_by_field_name("function") else {
        return false;
    };
    let Some(as_str_call) = fmt_field.child_by_field_name("value") else {
        return false;
    };
    let Some(as_str_field) = as_str_call.child_by_field_name("function") else {
        return false;
    };
    call.kind() == "call_expression"
        && fmt_field.kind() == "field_expression"
        && field_name(fmt_field, source) == Some("fmt")
        && as_str_call.kind() == "call_expression"
        && as_str_call
            .child_by_field_name("arguments")
            .is_some_and(|arguments| arguments.named_child_count() == 0)
        && as_str_field.kind() == "field_expression"
        && field_name(as_str_field, source) == Some("as_str")
        && as_str_field
            .child_by_field_name("value")
            .is_some_and(|value| value.kind() == "self")
}

fn mappings_match_case(mappings: &BTreeMap<String, String>, case: StringCase) -> bool {
    mappings.iter().all(|(variant, value)| match case {
        StringCase::Kebab => variant.to_kebab_case() == *value,
        StringCase::Snake => variant.to_snake_case() == *value,
    })
}

fn field_name<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let field = node.child_by_field_name("field")?;
    std::str::from_utf8(&source[field.byte_range()]).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
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

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a [u8]) -> Option<&'a str> {
    node_text(node.child_by_field_name(field)?, source)
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}
