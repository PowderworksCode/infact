//! Typed fact-pack loading and analysis entry point for Infact consumers.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use entl_semantics::SemanticObservations;
use entl_tree_sitter::ParserCatalog;
use infact_core::{
    CallEffectCatalog, DerivedLibraryBehavior, DerivedMacroBehavior, EffectTrace, ErrorDiscard,
    ExactTokenClone, ExternalCatalog, Fact, LibraryBehaviorMatch, NearTokenClone,
};
use infact_duplication::{ExactConfig, NearConfig};
use infact_fact_pack::{CachedFactPack, FactPackManifest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactPackSet {
    manifests: Vec<FactPackManifest>,
    catalogs: Vec<ExternalCatalog>,
    behaviors: Vec<DerivedLibraryBehavior>,
    macro_behaviors: Vec<DerivedMacroBehavior>,
    call_effects: Vec<CallEffectCatalog>,
    capabilities: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalysisSelection {
    pub exact_clones: Option<ExactConfig>,
    pub near_clones: Option<NearConfig>,
    pub library_behaviors: bool,
    pub call_effects: bool,
    pub error_discards: bool,
}

/// How the effect call graph was resolved.
///
/// A consumer needs this to say what a clean report means. Syntax resolution
/// cannot see a call written through an import, so "no effects found" is a
/// weaker claim than it looks; resolved observations make it a real one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectResolution {
    /// No effect analysis ran.
    #[default]
    None,
    /// Call destinations were guessed from syntax.
    Syntax,
    /// Call destinations came from a compiler that had already resolved them.
    Observed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactBatch {
    pub exact_clones: Vec<Fact<ExactTokenClone>>,
    pub near_clones: Vec<Fact<NearTokenClone>>,
    pub library_behaviors: Vec<Fact<LibraryBehaviorMatch>>,
    pub call_effects: Vec<CallEffectCatalog>,
    pub effect_traces: Vec<Fact<EffectTrace>>,
    #[serde(default)]
    pub error_discards: Vec<Fact<ErrorDiscard>>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub effect_resolution: EffectResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub analyzer: String,
    pub path: PathBuf,
    #[serde(default = "default_line")]
    pub line: u32,
    pub message: String,
}

const fn default_line() -> u32 {
    1
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("reading fact-pack content {}: {source}", path.display())]
    ReadContent {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decoding {kind} fact-pack content {}: {source}", path.display())]
    DecodeContent {
        kind: String,
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("fact pack {pack} requires unavailable capability {capability}")]
    MissingCapability { pack: String, capability: String },
    #[error(transparent)]
    Duplication(#[from] infact_duplication::Error),
    #[error(transparent)]
    RustBehaviors(#[from] infact_rust_behaviors::Error),
    #[error(transparent)]
    RustEffects(#[from] infact_rust_effects::Error),
    #[error(transparent)]
    RustErrors(#[from] infact_rust_errors::Error),
}

impl FactPackSet {
    pub fn load(packs: &[CachedFactPack]) -> Result<Self, Error> {
        let mut set = Self::default();
        for pack in packs {
            set.capabilities.extend(pack.manifest.provides.clone());
            set.manifests.push(pack.manifest.clone());
            for content in &pack.contents {
                match content.kind.as_str() {
                    "callable-signatures" => set
                        .catalogs
                        .push(read_json(&content.blob_path, &content.kind)?),
                    "library-behavior" => set
                        .behaviors
                        .push(read_json(&content.blob_path, &content.kind)?),
                    "macro-behavior" => set
                        .macro_behaviors
                        .push(read_json(&content.blob_path, &content.kind)?),
                    "call-effects" => set
                        .call_effects
                        .push(read_json(&content.blob_path, &content.kind)?),
                    _ => {}
                }
            }
        }
        set.manifests.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.subject.version.cmp(&right.subject.version))
                .then(left.revision.cmp(&right.revision))
        });
        set.catalogs.sort();
        set.behaviors.sort();
        set.macro_behaviors.sort();
        set.call_effects.sort();
        Ok(set)
    }

    pub fn manifests(&self) -> &[FactPackManifest] {
        &self.manifests
    }

    pub fn capabilities(&self) -> &BTreeSet<String> {
        &self.capabilities
    }

    pub fn call_effects(&self) -> &[CallEffectCatalog] {
        &self.call_effects
    }

    pub fn validate_runtime(&self, parsers: &ParserCatalog) -> Result<(), Error> {
        let mut available = self.capabilities.clone();
        for parser in parsers.iter() {
            available.insert(format!("{}.syntax-tree", parser.language().id));
        }
        for manifest in &self.manifests {
            for requirement in &manifest.requires {
                if !available.contains(requirement) {
                    return Err(Error::MissingCapability {
                        pack: manifest.name.clone(),
                        capability: requirement.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

pub fn analyze_repository(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    packs: &FactPackSet,
    selection: &AnalysisSelection,
) -> Result<FactBatch, Error> {
    analyze_repository_with_observations(root, parsers, packs, selection, &[])
}

/// Analyze a repository, using resolved semantic observations where they exist.
///
/// Observations sharpen the result; syntax is the floor. A repository with no
/// observations, in a language with no provider, or one that will not compile
/// still gets analyzed — less precisely, and the diagnostics say so.
pub fn analyze_repository_with_observations(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    packs: &FactPackSet,
    selection: &AnalysisSelection,
    observations: &[SemanticObservations],
) -> Result<FactBatch, Error> {
    packs.validate_runtime(parsers)?;
    let root = root.as_ref();
    let mut batch = FactBatch::default();
    if selection.call_effects {
        batch.call_effects.clone_from(&packs.call_effects);
        let resolved = SemanticObservations::merge(observations.to_vec(), "repository")
            .filter(|merged| merged.coverage.call_edges);
        let report = match &resolved {
            Some(merged) => {
                infact_rust_effects::analyze_observed_effects(merged, &packs.call_effects)?
            }
            None => {
                infact_rust_effects::analyze_repository_effects(root, parsers, &packs.call_effects)?
            }
        };
        batch.effect_resolution = if resolved.is_some() {
            EffectResolution::Observed
        } else {
            EffectResolution::Syntax
        };
        batch.effect_traces = report.effects;
        batch
            .diagnostics
            .extend(
                report
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| AnalysisDiagnostic {
                        analyzer: "effects".to_owned(),
                        path: diagnostic.path,
                        line: diagnostic.line,
                        message: diagnostic.message,
                    }),
            );
    }
    if selection.error_discards {
        let report = infact_rust_errors::analyze_repository_errors(root, parsers)?;
        batch.error_discards = report.discards;
        batch
            .diagnostics
            .extend(
                report
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| AnalysisDiagnostic {
                        analyzer: "error-discards".to_owned(),
                        path: diagnostic.path,
                        line: diagnostic.line,
                        message: diagnostic.message,
                    }),
            );
    }
    if let Some(config) = selection.exact_clones {
        let report = infact_duplication::analyze_repository_with_catalog(root, parsers, config)?;
        batch.exact_clones = report.clones;
        batch
            .diagnostics
            .extend(
                report
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| AnalysisDiagnostic {
                        analyzer: "exact-clones".to_owned(),
                        path: diagnostic.path,
                        line: 1,
                        message: diagnostic.message,
                    }),
            );
    }
    if let Some(config) = selection.near_clones {
        let report =
            infact_duplication::analyze_repository_near_with_catalog(root, parsers, config)?;
        batch.near_clones = report.clones;
        batch
            .diagnostics
            .extend(
                report
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| AnalysisDiagnostic {
                        analyzer: "near-clones".to_owned(),
                        path: diagnostic.path,
                        line: 1,
                        message: diagnostic.message,
                    }),
            );
    }
    if selection.library_behaviors {
        let report = infact_rust_behaviors::analyze_repository(
            root,
            parsers,
            &packs.catalogs,
            &packs.behaviors,
            &packs.macro_behaviors,
        )?;
        batch.library_behaviors = report.matches;
        batch
            .diagnostics
            .extend(
                report
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| AnalysisDiagnostic {
                        analyzer: "library-behaviors".to_owned(),
                        path: diagnostic.path,
                        line: 1,
                        message: diagnostic.message,
                    }),
            );
    }
    batch.diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.analyzer.cmp(&right.analyzer))
            .then(left.message.cmp(&right.message))
    });
    batch.diagnostics.dedup();
    Ok(batch)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, kind: &str) -> Result<T, Error> {
    let source = fs::read(path).map_err(|source| Error::ReadContent {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&source).map_err(|source| Error::DecodeContent {
        kind: kind.to_owned(),
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use infact_fact_pack::{FactPackCache, FactPackManifest, build_oci_layout};

    fn install_pack(name: &str, cache: &FactPackCache) -> CachedFactPack {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("infact-packs")
            .join(name);
        let manifest =
            FactPackManifest::parse(&fs::read_to_string(root.join("pack.toml")).unwrap()).unwrap();
        let output_root = tempfile::tempdir().unwrap();
        let layout = output_root.path().join("layout");
        build_oci_layout(&manifest, &root, &layout).unwrap();
        cache.import_oci_layout(layout).unwrap()
    }

    fn effects_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../infact-rust-effects/tests/fixtures/imports")
    }

    fn parsers() -> ParserCatalog {
        let discovery = ParserCatalog::discover([
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs")
        ]);
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        discovery.catalog
    }

    fn effect_packs(cache: &FactPackCache) -> FactPackSet {
        FactPackSet::load(&[install_pack("rust-core", cache)]).unwrap()
    }

    /// Observations sharpen the analysis; without them it still runs.
    #[test]
    fn observations_replace_syntax_resolution_when_they_are_available() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let packs = effect_packs(&cache);
        let parsers = parsers();
        let selection = AnalysisSelection {
            call_effects: true,
            ..AnalysisSelection::default()
        };

        let syntax = analyze_repository(effects_fixture(), &parsers, &packs, &selection).unwrap();
        assert_eq!(syntax.effect_resolution, EffectResolution::Syntax);

        let observed_input: SemanticObservations =
            read_json(&effects_fixture().join("observations.json"), "observations").unwrap();
        let observed = analyze_repository_with_observations(
            effects_fixture(),
            &parsers,
            &packs,
            &selection,
            &[observed_input],
        )
        .unwrap();
        assert_eq!(observed.effect_resolution, EffectResolution::Observed);

        // the import spellings only resolve on the observed path
        let callables = |batch: &FactBatch| {
            let mut names = batch
                .effect_traces
                .iter()
                .map(|fact| fact.value.callable.clone())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            names
        };
        assert!(
            callables(&observed).len() > callables(&syntax).len(),
            "syntax {:?} should see less than observed {:?}",
            callables(&syntax),
            callables(&observed)
        );
        assert!(
            callables(&observed)
                .iter()
                .any(|name| name.ends_with("via_module"))
        );
    }

    /// Observations that never looked for calls must not be trusted for calls.
    #[test]
    fn observations_without_call_coverage_fall_back_to_syntax() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let packs = effect_packs(&cache);
        let mut observations: SemanticObservations =
            read_json(&effects_fixture().join("observations.json"), "observations").unwrap();
        observations.coverage.call_edges = false;

        let batch = analyze_repository_with_observations(
            effects_fixture(),
            &parsers(),
            &packs,
            &AnalysisSelection {
                call_effects: true,
                ..AnalysisSelection::default()
            },
            &[observations],
        )
        .unwrap();
        assert_eq!(batch.effect_resolution, EffectResolution::Syntax);
    }

    #[test]
    fn loads_verified_pack_contents_and_runs_behavior_analysis() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let packs = FactPackSet::load(&[
            install_pack("rust-core", &cache),
            install_pack("rust-itertools", &cache),
            install_pack("rust-strum", &cache),
        ])
        .unwrap();
        assert!(packs.capabilities().contains("rust.call-effects"));
        assert!(packs.call_effects().iter().any(|catalog| {
            catalog
                .calls
                .iter()
                .any(|call| call.path == "std::fs::write")
        }));
        let parsers = ParserCatalog::discover([
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs")
        ]);
        assert!(parsers.errors.is_empty());
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../infact-rust-behaviors/tests/fixtures/counts");
        let batch = analyze_repository(
            fixture,
            &parsers.catalog,
            &packs,
            &AnalysisSelection {
                library_behaviors: true,
                call_effects: true,
                ..AnalysisSelection::default()
            },
        )
        .unwrap();
        assert!(!batch.library_behaviors.is_empty());
        assert!(!batch.call_effects.is_empty());
        assert!(batch.exact_clones.is_empty());
        assert!(batch.near_clones.is_empty());
    }
}
