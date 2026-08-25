//! Facts matching TypeScript and JavaScript code behavior to library APIs.
//!
//! The Rust crate beside this one and this one differ only in how a language
//! spells a function. Everything about what a match *means* — following a
//! callable to the body that does its work, whether a form is specific enough
//! to report, which of several colliding APIs to name — is shared, and lives in
//! `infact-behaviors` and `infact-normalize`.

mod derivation;
mod pack;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use infact_core::{
    DERIVED_LIBRARY_BEHAVIOR_SCHEMA, Derivation, DerivedLibraryBehavior, EXTERNAL_CATALOG_SCHEMA,
    ExternalCatalog, Fact, Form, InputEvidence, LibraryBehaviorMatch, LibraryTarget, SourceSpan,
};
use infact_ts_normalize::StatementSpan;
use tree_sitter::Node;

pub use derivation::{DerivedLibrary, derive_library};
pub use pack::{BuiltLibraryPack, LibraryPackRequest, build_library_pack};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    #[error(
        "external catalog for {package} {version} uses schema {actual}; supported schema is {expected}"
    )]
    UnsupportedCatalogSchema {
        package: String,
        version: String,
        actual: u32,
        expected: u32,
    },
    #[error("derived behavior for {callable} uses schema {actual}; supported schema is {expected}")]
    UnsupportedBehaviorSchema {
        callable: String,
        actual: u32,
        expected: u32,
    },
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

pub type Result<T> = std::result::Result<T, Error>;

/// One unit of repository code to compare against the library.
///
/// A function is the obvious unit. A file's top level is the other one, and it
/// is not optional: JavaScript and TypeScript let work sit outside any function,
/// and a matcher that only looked inside functions would report nothing about it
/// — silently, which is the failure worth going out of the way to avoid.
struct Unit {
    form: Form,
    statements: Vec<StatementSpan>,
    start_byte: u64,
    end_byte: u64,
    start_line: u32,
    end_line: u32,
}

fn units(file: &ParsedFile) -> Vec<Unit> {
    let root = file.tree.root_node();
    let (form, statements) = infact_ts_normalize::normalize_module_located(file);
    let mut units = vec![Unit {
        form,
        statements,
        start_byte: root.start_byte() as u64,
        end_byte: root.end_byte() as u64,
        start_line: u32::try_from(root.start_position().row + 1).unwrap_or(u32::MAX),
        end_line: u32::try_from(root.end_position().row + 1).unwrap_or(u32::MAX),
    }];
    units.extend(
        infact_ts_normalize::normalize_file(file)
            .into_iter()
            // A function the parser could not read whole is not a function whose
            // body says what this one says.
            .filter(|function| !function.damaged)
            .map(|function| Unit {
                form: function.form,
                statements: function.statements,
                start_byte: function.start_byte,
                end_byte: function.end_byte,
                start_line: function.start_line,
                end_line: function.end_line,
            }),
    );
    units
}

/// Match repository code against derived library behaviors.
///
/// Every behavior is compared the same way: normalize the repository code, then
/// look for the behavior's form inside it. Nothing here knows which library or
/// which API is being matched, so a newly derived behavior becomes matchable
/// without any code changing.
pub fn analyze_repository(
    root: impl AsRef<std::path::Path>,
    parsers: &ParserCatalog,
    catalogs: &[ExternalCatalog],
    behaviors: &[DerivedLibraryBehavior],
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

    // A behavior is only worth matching if it describes enough to be specific.
    // Below the floor, unrelated code collides: getters, one-line delegations,
    // and property accessors all reduce to the same handful of shapes.
    let reportable = behaviors
        .iter()
        .filter(|behavior| behavior.program.is_reportable())
        .collect::<Vec<_>>();

    let parsed = parse_repository(root, parsers)?;
    let mut matches = BTreeSet::new();
    for file in &parsed.files {
        if !derivation::is_ecmascript(file) {
            continue;
        }
        for unit in units(file) {
            let candidate = unit.form.simplify();
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
                // highest up the delegation chain: what the library offers for
                // this, not how it is built.
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
                let mut located = unit.form.locate_all(&behavior.program);
                if located.is_empty() {
                    located = candidate.locate_all(&behavior.program);
                }
                // A behavior that matched but cannot be placed is still a
                // finding; it just has no statement to point at.
                let placements = if located.is_empty() {
                    vec![None]
                } else {
                    located.into_iter().map(Some).collect()
                };
                for steps in placements {
                    matches.insert(behavior_match(
                        file,
                        &unit,
                        steps,
                        fused,
                        catalog,
                        behavior,
                        alternatives.clone(),
                    ));
                }
            }
        }
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
/// Entl records a file's digest as bare hex; a derived behavior records the same
/// digest prefixed with its algorithm, so the comparison ignores that.
fn derived_from(behavior: &DerivedLibraryBehavior, file: &ParsedFile) -> bool {
    behavior.implementation.iter().any(|evidence| {
        evidence
            .source_sha256
            .rsplit(':')
            .next()
            .is_some_and(|digest| digest == file.provenance.source_sha256)
    })
}

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

/// Report a match at the statements that carry it.
///
/// A behavior usually occupies a run of consecutive statements inside something
/// larger, and naming the function alone leaves a reader to find it again. When
/// the run cannot be located the enclosing unit is the honest answer.
fn behavior_match(
    file: &ParsedFile,
    unit: &Unit,
    located: Option<std::ops::Range<usize>>,
    fused: bool,
    catalog: &ExternalCatalog,
    behavior: &DerivedLibraryBehavior,
    alternatives: Vec<LibraryTarget>,
) -> Fact<LibraryBehaviorMatch> {
    let span = located
        .and_then(|steps| {
            let first = unit.statements.get(steps.start)?;
            let last = unit.statements.get(steps.end.checked_sub(1)?)?;
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
            start_byte: Some(unit.start_byte),
            end_byte: Some(unit.end_byte),
            start_line: unit.start_line,
            end_line: unit.end_line,
            start_column: None,
            end_column: None,
        });
    Fact {
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
            conditions: Vec::new(),
        },
        derivation: Derivation {
            analyzer: "typescript.library-behaviors".to_owned(),
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
    }
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
