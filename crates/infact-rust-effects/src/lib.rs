//! Source-backed effect summaries for selected Rust standard-library modules.

mod allocation;
mod evidence;
mod observed;
mod path;

use allocation::collect_allocating_macros;
use evidence::{evidence_path, evidence_paths};
pub use observed::{analyze_observed_effects, unexplained_destinations};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime, parse_repository};
use infact_core::{
    CALL_EFFECT_CATALOG_SCHEMA, CallEffectCatalog, CallEffects, Derivation as FactDerivation,
    Effect, EffectTrace, Fact, InputEvidence, SourceSpan,
};
use infact_fact_pack::{
    BuiltLayout, Compatibility, Compiler, Content, Derivation, FACT_PACK_SCHEMA, FactPackManifest,
    ManifestError, SourceInput, SourceKind, Subject, SubjectKind, build_oci_layout, sha256,
};
use sha2::{Digest, Sha256};
use tree_sitter::Node;

const MODULES: &[ModuleSpec] = &[
    ModuleSpec::new("std/src/env.rs", "std::env"),
    ModuleSpec::new("std/src/fs.rs", "std::fs"),
    ModuleSpec::new("std/src/net/tcp.rs", "std::net"),
    ModuleSpec::new("std/src/process.rs", "std::process"),
    ModuleSpec::new("std/src/thread/functions.rs", "std::thread"),
    ModuleSpec::new("std/src/time.rs", "std::time"),
];

#[derive(Debug, Clone, Copy)]
struct ModuleSpec {
    source: &'static str,
    module: &'static str,
}

impl ModuleSpec {
    const fn new(source: &'static str, module: &'static str) -> Self {
        Self { source, module }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationReport {
    pub catalog: CallEffectCatalog,
    pub source_sha256: String,
    pub files_parsed: usize,
    pub callables: usize,
    pub public_callables: usize,
    pub calls: CallAccounting,
    pub direct_seeds: usize,
}

pub struct RustStdFactPackRequest<'a> {
    pub library_root: &'a Path,
    pub version: &'a str,
    pub compiler_commit: Option<String>,
    pub revision: u32,
    pub parsers: &'a ParserCatalog,
    pub output: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltRustStdFactPack {
    pub manifest: FactPackManifest,
    pub layout: BuiltLayout,
    pub report: DerivationReport,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallAccounting {
    pub total: usize,
    pub linked_internal: usize,
    pub known_effect_origins: usize,
    pub constructors: usize,
    pub outside_selected_corpus: usize,
    pub dynamic_or_ambiguous: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepositoryEffectDiagnostic {
    pub path: PathBuf,
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryEffectReport {
    pub effects: Vec<Fact<EffectTrace>>,
    pub diagnostics: Vec<RepositoryEffectDiagnostic>,
    pub calls: CallAccounting,
}

impl CallAccounting {
    pub fn unlinked(&self) -> usize {
        self.total.saturating_sub(self.linked_internal)
    }

    fn accounted(&self) -> usize {
        self.linked_internal
            + self.known_effect_origins
            + self.constructors
            + self.outside_selected_corpus
            + self.dynamic_or_ambiguous
            + self.unknown
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no Rust parser pack is configured")]
    MissingRustParser,
    #[error("loading Rust parser pack: {0}")]
    Parser(#[from] entl_tree_sitter::Error),
    #[error("reading Rust standard-library source {}: {source}", path.display())]
    ReadSource {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Rust standard-library source contains syntax errors: {}", path.display())]
    Syntax { path: PathBuf },
    #[error("source file {path} is too large for source coordinates")]
    SourceTooLarge { path: PathBuf },
    #[error("serializing Rust standard-library effect catalog: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("writing generated fact-pack file {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("building generated fact pack: {0}")]
    FactPack(#[from] ManifestError),
    #[error("creating generated fact-pack directory: {0}")]
    Temporary(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Derive effect traces through the Rust callables in a repository.
///
/// External effect seeds come from verified call-effect catalogs. Local call
/// edges are syntax-resolved, and their transitive closure is a Datalog rule.
pub fn analyze_repository_effects(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalogs: &[CallEffectCatalog],
) -> Result<RepositoryEffectReport> {
    let parsed = parse_repository(root, parsers)?;
    let mut callables = Vec::new();
    let mut inputs = BTreeMap::new();
    for file in &parsed.files {
        if file.pack.language().id != "rust" {
            continue;
        }
        inputs.insert(file.path.clone(), input_evidence(file));
        collect_callables(
            file.tree.root_node(),
            &file.source,
            &file.path,
            &repository_module(&file.path),
            None,
            None,
            &mut callables,
        )?;
    }
    callables.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.span.path.cmp(&right.span.path))
            .then(left.span.start_byte.cmp(&right.span.start_byte))
    });
    for (index, callable) in callables.iter_mut().enumerate() {
        callable.id = u64::try_from(index).expect("callable index fits in u64");
    }

    let external = external_effects(catalogs);
    let mut calls = Vec::new();
    let mut seeds = Vec::new();
    let mut unresolved = Vec::new();
    let mut accounting = CallAccounting::default();
    for callable in &callables {
        if callable.is_unsafe {
            seeds.push(EffectSeed {
                callable: callable.id,
                effect: Effect::Unsafe,
                origin: "rust:unsafe-function".to_owned(),
                span: callable.span.clone(),
            });
        }
        collect_repository_calls(
            callable,
            &callables,
            &external,
            &mut calls,
            &mut seeds,
            &mut unresolved,
            &mut accounting,
        );
    }
    calls.sort();
    calls.dedup();
    seeds.sort();
    seeds.dedup();

    let propagated = propagate_effects(&calls, &seeds);
    let calls_by_caller = calls.iter().fold(
        BTreeMap::<u64, Vec<&ResolvedCall>>::new(),
        |mut by_caller, call| {
            by_caller.entry(call.caller).or_default().push(call);
            by_caller
        },
    );
    let seeds_by_callable = seeds.iter().fold(
        BTreeMap::<u64, Vec<&EffectSeed>>::new(),
        |mut by_callable, seed| {
            by_callable.entry(seed.callable).or_default().push(seed);
            by_callable
        },
    );
    let callable_by_id = callables
        .iter()
        .map(|callable| (callable.id, callable))
        .collect::<BTreeMap<_, _>>();
    // the evidence search needs only names
    let paths_by_id = callables
        .iter()
        .map(|callable| (callable.id, callable.path.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut effects = Vec::new();
    let mut effectful = BTreeSet::new();
    for relation in propagated {
        let effect = decode_effect(relation.effect);
        let Some(callable) = callable_by_id.get(&relation.callable) else {
            continue;
        };
        // A callable that does the thing twelve times has twelve traces. One
        // per site, so fixing what you were shown does not leave eleven behind.
        let reached = evidence_paths(
            callable.id,
            effect,
            &calls_by_caller,
            &seeds_by_callable,
            &paths_by_id,
        );
        if reached.is_empty() {
            continue;
        }
        effectful.insert(callable.id);
        for evidence in reached {
            let value = EffectTrace {
                callable: callable.path.clone(),
                callable_span: callable.span.clone(),
                effect,
                origin: evidence.origin,
                path: evidence.path,
            };
            effects.push(Fact {
                derivation: trace_derivation(&value, &inputs),
                value,
            });
        }
    }
    effects.sort();
    effects.dedup();

    let mut diagnostics = parsed
        .diagnostics
        .into_iter()
        .map(|diagnostic| RepositoryEffectDiagnostic {
            path: diagnostic.path,
            line: 1,
            message: diagnostic.message,
        })
        .collect::<Vec<_>>();
    for (caller_id, syntax_call) in unresolved {
        let Some(caller) = callable_by_id.get(&caller_id) else {
            continue;
        };
        let Some(name) = trailing_identifier(&syntax_call.callee) else {
            continue;
        };
        let candidates = callables
            .iter()
            .filter(|candidate| candidate.name == name && effectful.contains(&candidate.id))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            continue;
        }
        diagnostics.push(RepositoryEffectDiagnostic {
            path: syntax_call.span.path.clone(),
            line: syntax_call.span.start_line,
            message: format!(
                "call `{}` in {} may target an effectful local callable but could not be resolved uniquely",
                syntax_call.callee, caller.path
            ),
        });
    }
    diagnostics.sort();
    diagnostics.dedup();
    debug_assert_eq!(accounting.total, accounting.accounted());

    Ok(RepositoryEffectReport {
        effects,
        diagnostics,
        calls: accounting,
    })
}

/// Derive public API effect summaries from a Rust checkout's `library` directory.
///
/// This deliberately covers only a small collection of `std` modules. Boundary
/// classification is explicit; wrapper propagation is computed from syntax-extracted
/// call edges.
pub fn derive_std_effects(
    library_root: impl AsRef<Path>,
    version: impl Into<String>,
    parsers: &ParserCatalog,
) -> Result<DerivationReport> {
    let library_root = library_root.as_ref();
    let runtime = ParserRuntime::new()?;
    let rust_pack = parsers
        .resolve("rust", Path::new("source.rs"))
        .ok_or(Error::MissingRustParser)?;
    let parser = runtime.load(Arc::clone(rust_pack))?;

    let mut callables = Vec::new();
    let mut source_hasher = Sha256::new();
    source_hasher.update(b"infact-rust-std-selected-sources-v1\0");
    for module in MODULES {
        let relative = PathBuf::from(module.source);
        let absolute = library_root.join(&relative);
        let source = std::fs::read(&absolute).map_err(|source| Error::ReadSource {
            path: absolute,
            source,
        })?;
        hash_named_input(&mut source_hasher, module.source, &source);
        let parsed = parser.parse(relative.clone(), Arc::<[u8]>::from(source))?;
        if parsed.tree.root_node().has_error() {
            return Err(Error::Syntax { path: relative });
        }
        collect_callables(
            parsed.tree.root_node(),
            &parsed.source,
            &parsed.path,
            module.module,
            None,
            None,
            &mut callables,
        )?;
    }
    callables.sort_by(|left, right| left.path.cmp(&right.path));
    for (index, callable) in callables.iter_mut().enumerate() {
        callable.id = u64::try_from(index).expect("callable index fits in u64");
    }

    let mut calls = Vec::new();
    let mut seeds = Vec::new();
    let mut call_accounting = CallAccounting::default();
    for callable in &callables {
        for (effect, origin) in declaration_effects(&callable.path) {
            seeds.push(EffectSeed {
                callable: callable.id,
                effect,
                origin: origin.to_owned(),
                span: callable.span.clone(),
            });
        }
        if callable.is_unsafe {
            seeds.push(EffectSeed {
                callable: callable.id,
                effect: Effect::Unsafe,
                origin: "rust:unsafe-function".to_owned(),
                span: callable.span.clone(),
            });
        }
        collect_calls(
            callable,
            &callables,
            &mut calls,
            &mut seeds,
            &mut call_accounting,
        )?;
    }
    calls.sort();
    calls.dedup();
    seeds.sort();
    seeds.dedup();

    let propagated = propagate_effects(&calls, &seeds);
    let calls_by_caller = calls.iter().fold(
        BTreeMap::<u64, Vec<&ResolvedCall>>::new(),
        |mut by_caller, call| {
            by_caller.entry(call.caller).or_default().push(call);
            by_caller
        },
    );
    let seeds_by_callable = seeds.iter().fold(
        BTreeMap::<u64, Vec<&EffectSeed>>::new(),
        |mut by_callable, seed| {
            by_callable.entry(seed.callable).or_default().push(seed);
            by_callable
        },
    );
    let callable_by_id = callables
        .iter()
        .map(|callable| (callable.id, callable))
        .collect::<BTreeMap<_, _>>();
    // the evidence search needs only names
    let paths_by_id = callables
        .iter()
        .map(|callable| (callable.id, callable.path.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut summaries = BTreeMap::<String, Vec<Effect>>::new();
    for relation in propagated {
        let Some(callable) = callable_by_id.get(&relation.callable) else {
            continue;
        };
        if callable.is_public {
            summaries
                .entry(callable.path.clone())
                .or_default()
                .push(decode_effect(relation.effect));
        }
    }

    let calls = summaries
        .into_iter()
        .map(|(path, mut effects)| {
            effects.sort();
            effects.dedup();
            let callable = callables
                .iter()
                .find(|callable| callable.path == path)
                .expect("summary originates from a callable");
            let evidence = effects
                .iter()
                .filter_map(|effect| {
                    evidence_path(
                        callable.id,
                        *effect,
                        &calls_by_caller,
                        &seeds_by_callable,
                        &paths_by_id,
                    )
                })
                .collect();
            CallEffects {
                path,
                effects,
                evidence,
            }
        })
        .collect();

    debug_assert_eq!(call_accounting.total, call_accounting.accounted());
    Ok(DerivationReport {
        catalog: CallEffectCatalog {
            schema: CALL_EFFECT_CATALOG_SCHEMA,
            language: "rust".to_owned(),
            version: version.into(),
            calls,
        },
        source_sha256: format!("sha256:{:x}", source_hasher.finalize()),
        files_parsed: MODULES.len(),
        callables: callables.len(),
        public_callables: callables
            .iter()
            .filter(|callable| callable.is_public)
            .count(),
        calls: call_accounting,
        direct_seeds: seeds.len(),
    })
}

pub fn build_std_fact_pack(request: RustStdFactPackRequest<'_>) -> Result<BuiltRustStdFactPack> {
    let report = derive_std_effects(request.library_root, request.version, request.parsers)?;
    let temporary = tempfile::tempdir().map_err(Error::Temporary)?;
    let content_path = format!("effects/rust-core-{}.json", request.version);
    let absolute_content = temporary.path().join(&content_path);
    let content_directory = absolute_content
        .parent()
        .expect("generated content path has a parent");
    std::fs::create_dir_all(content_directory).map_err(|source| Error::Write {
        path: content_directory.to_path_buf(),
        source,
    })?;
    let mut catalog_json = serde_json::to_vec_pretty(&report.catalog)?;
    catalog_json.push(b'\n');
    std::fs::write(&absolute_content, &catalog_json).map_err(|source| Error::Write {
        path: absolute_content.clone(),
        source,
    })?;

    let manifest = FactPackManifest {
        schema: FACT_PACK_SCHEMA,
        name: "rust-core".to_owned(),
        revision: request.revision,
        subject: Subject {
            kind: SubjectKind::Language,
            language: "rust".to_owned(),
            ecosystem: Some("cargo".to_owned()),
            name: "core".to_owned(),
            version: request.version.to_owned(),
        },
        sources: vec![SourceInput {
            kind: SourceKind::Toolchain,
            name: "rust".to_owned(),
            version: request.version.to_owned(),
            sha256: report.source_sha256.clone(),
        }],
        derivation: Derivation {
            generator: "infact".to_owned(),
            generator_version: env!("CARGO_PKG_VERSION").to_owned(),
            analyzer_sha256: analyzer_sha256(),
        },
        compatibility: Compatibility {
            compiler: Some(Compiler {
                name: "rustc".to_owned(),
                version: request.version.to_owned(),
                commit: request.compiler_commit,
            }),
            ..Compatibility::default()
        },
        provides: BTreeSet::from(["rust.call-effects".to_owned()]),
        requires: BTreeSet::new(),
        contents: vec![Content {
            path: content_path,
            kind: "call-effects".to_owned(),
            media_type: "application/vnd.infact.call-effects.v1+json".to_owned(),
            sha256: sha256(&catalog_json),
        }],
    };
    let manifest_path = temporary.path().join("pack.toml");
    std::fs::write(&manifest_path, manifest.to_canonical_toml()?).map_err(|source| {
        Error::Write {
            path: manifest_path,
            source,
        }
    })?;
    let layout = build_oci_layout(&manifest, temporary.path(), request.output)?;
    Ok(BuiltRustStdFactPack {
        manifest,
        layout,
        report,
    })
}

pub fn analyzer_sha256() -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"infact-rust-effects-analyzer-v1\0");
    hash_named_input(&mut hasher, "Cargo.toml", include_bytes!("../Cargo.toml"));
    hash_named_input(&mut hasher, "src/lib.rs", include_bytes!("lib.rs"));
    format!("sha256:{:x}", hasher.finalize())
}

fn hash_named_input(hasher: &mut Sha256, path: &str, bytes: &[u8]) {
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[derive(Debug, Clone)]
struct Callable {
    id: u64,
    path: String,
    name: String,
    module: String,
    parent: Option<String>,
    is_public: bool,
    is_unsafe: bool,
    span: SourceSpan,
    syntax_calls: Vec<SyntaxCall>,
    /// Allocating macro expansions, which are not call expressions and so are
    /// invisible to the call walk. Collected for every callable; only the
    /// repository analysis reads them, because the standard-library derivation
    /// takes its origins from explicit rules rather than from allocation.
    allocating_macros: Vec<SyntaxCall>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SyntaxCall {
    callee: String,
    span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResolvedCall {
    caller: u64,
    callee: u64,
    span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EffectSeed {
    callable: u64,
    effect: Effect,
    origin: String,
    span: SourceSpan,
}

#[allow(clippy::too_many_arguments)]
fn collect_callables(
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    module: &str,
    implementation: Option<String>,
    parent: Option<String>,
    output: &mut Vec<Callable>,
) -> Result<()> {
    if node.kind() == "impl_item" {
        let implementation = node
            .child_by_field_name("type")
            .and_then(|node| node_text(node, source))
            .map(simple_type_name);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_callables(
                child,
                source,
                path,
                module,
                implementation.clone(),
                parent.clone(),
                output,
            )?;
        }
        return Ok(());
    }

    if node.kind() == "function_item" {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        let Some(name) = node_text(name_node, source) else {
            return Ok(());
        };
        let line = node.start_position().row + 1;
        let callable_path = if let Some(parent) = &parent {
            format!("{parent}::{name}@{line}")
        } else if let Some(implementation) = &implementation {
            format!("{module}::{implementation}::{name}")
        } else {
            format!("{module}::{name}")
        };
        let body = node.child_by_field_name("body");
        let header_end = body.map_or(node.end_byte(), |body| body.start_byte());
        let header_bytes = source
            .get(node.start_byte()..header_end)
            .unwrap_or_default();
        let header = std::str::from_utf8(header_bytes).unwrap_or_default();
        let span = source_span(path, node)?;
        let mut syntax_calls = Vec::new();
        let mut allocating_macros = Vec::new();
        if let Some(body) = body {
            collect_syntax_calls(body, body, source, path, &mut syntax_calls)?;
            collect_allocating_macros(body, body, source, path, &mut allocating_macros)?;
        }
        output.push(Callable {
            id: 0,
            path: callable_path.clone(),
            name: name.to_owned(),
            module: module.to_owned(),
            parent: parent.clone(),
            is_public: is_public_header(header),
            is_unsafe: header.split_whitespace().any(|token| token == "unsafe"),
            span,
            syntax_calls,
            allocating_macros,
        });
        if let Some(body) = body {
            let mut cursor = body.walk();
            for child in body.named_children(&mut cursor) {
                collect_callables(
                    child,
                    source,
                    path,
                    module,
                    implementation.clone(),
                    Some(callable_path.clone()),
                    output,
                )?;
            }
        }
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_callables(
            child,
            source,
            path,
            module,
            implementation.clone(),
            parent.clone(),
            output,
        )?;
    }
    Ok(())
}

fn collect_syntax_calls(
    root: Node<'_>,
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    output: &mut Vec<SyntaxCall>,
) -> Result<()> {
    if node != root && node.kind() == "function_item" {
        return Ok(());
    }
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && let Some(callee) = node_text(function, source)
    {
        output.push(SyntaxCall {
            callee: normalize_callee(callee),
            span: source_span(path, node)?,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_syntax_calls(root, child, source, path, output)?;
    }
    Ok(())
}

fn collect_calls(
    callable: &Callable,
    callables: &[Callable],
    calls: &mut Vec<ResolvedCall>,
    seeds: &mut Vec<EffectSeed>,
    accounting: &mut CallAccounting,
) -> Result<()> {
    for syntax_call in &callable.syntax_calls {
        accounting.total += 1;
        let origin_effects = origin_effects(callable, &syntax_call.callee);
        for effect in &origin_effects {
            seeds.push(EffectSeed {
                callable: callable.id,
                effect: *effect,
                origin: syntax_call.callee.clone(),
                span: syntax_call.span.clone(),
            });
        }
        if !origin_effects.is_empty() {
            accounting.known_effect_origins += 1;
            continue;
        }
        if let Some(callee) = resolve_call(callable, syntax_call, callables) {
            calls.push(ResolvedCall {
                caller: callable.id,
                callee: callee.id,
                span: syntax_call.span.clone(),
            });
            accounting.linked_internal += 1;
        } else if is_constructor(&syntax_call.callee) {
            accounting.constructors += 1;
        } else if syntax_call.callee.contains('.') {
            accounting.dynamic_or_ambiguous += 1;
        } else if syntax_call.callee.contains("::") {
            accounting.outside_selected_corpus += 1;
        } else {
            accounting.unknown += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_repository_calls(
    callable: &Callable,
    callables: &[Callable],
    external: &BTreeMap<String, Vec<Effect>>,
    calls: &mut Vec<ResolvedCall>,
    seeds: &mut Vec<EffectSeed>,
    unresolved: &mut Vec<(u64, SyntaxCall)>,
    accounting: &mut CallAccounting,
) {
    for syntax_call in &callable.syntax_calls {
        accounting.total += 1;
        if let Some((origin, effects)) = external_call(&syntax_call.callee, external) {
            for effect in effects {
                seeds.push(EffectSeed {
                    callable: callable.id,
                    effect: *effect,
                    origin: origin.to_owned(),
                    span: syntax_call.span.clone(),
                });
            }
            accounting.known_effect_origins += 1;
            continue;
        }
        if let Some(seed) = allocation::call_seed(callable.id, syntax_call) {
            seeds.push(seed);
            accounting.known_effect_origins += 1;
            continue;
        }
        if let Some(callee) = resolve_call(callable, syntax_call, callables) {
            calls.push(ResolvedCall {
                caller: callable.id,
                callee: callee.id,
                span: syntax_call.span.clone(),
            });
            accounting.linked_internal += 1;
        } else if is_constructor(&syntax_call.callee) {
            accounting.constructors += 1;
        } else if syntax_call.callee.contains('.') {
            accounting.dynamic_or_ambiguous += 1;
            unresolved.push((callable.id, syntax_call.clone()));
        } else if syntax_call.callee.contains("::") {
            accounting.outside_selected_corpus += 1;
            unresolved.push((callable.id, syntax_call.clone()));
        } else {
            accounting.unknown += 1;
            unresolved.push((callable.id, syntax_call.clone()));
        }
    }
    seeds.extend(allocation::macro_seeds(
        callable.id,
        &callable.allocating_macros,
    ));
}

fn external_effects(catalogs: &[CallEffectCatalog]) -> BTreeMap<String, Vec<Effect>> {
    let mut external = BTreeMap::<String, Vec<Effect>>::new();
    for catalog in catalogs.iter().filter(|catalog| catalog.language == "rust") {
        for call in &catalog.calls {
            external
                .entry(call.path.clone())
                .or_default()
                .extend(call.effects.iter().copied());
        }
    }
    for effects in external.values_mut() {
        effects.sort();
        effects.dedup();
    }
    external
}

fn external_call<'a>(
    callee: &str,
    external: &'a BTreeMap<String, Vec<Effect>>,
) -> Option<(&'a str, &'a [Effect])> {
    external
        .iter()
        .find(|(path, _)| {
            callee == path.as_str()
                || callee
                    .strip_prefix(path.as_str())
                    .is_some_and(|suffix| suffix.starts_with("::<"))
        })
        .map(|(path, effects)| (path.as_str(), effects.as_slice()))
}

fn is_constructor(callee: &str) -> bool {
    let Some(name) = trailing_identifier(callee) else {
        return false;
    };
    name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
}

fn resolve_call<'a>(
    caller: &Callable,
    syntax_call: &SyntaxCall,
    callables: &'a [Callable],
) -> Option<&'a Callable> {
    let name = trailing_identifier(&syntax_call.callee)?;
    let nested = callables
        .iter()
        .filter(|candidate| {
            candidate.parent.as_deref() == Some(caller.path.as_str()) && candidate.name == name
        })
        .collect::<Vec<_>>();
    if nested.len() == 1 {
        return nested.into_iter().next();
    }

    let identifiers = identifiers(&syntax_call.callee);
    if identifiers.len() >= 2 {
        let short_path = format!(
            "{}::{}",
            identifiers[identifiers.len() - 2],
            identifiers[identifiers.len() - 1]
        );
        let suffix = format!("::{short_path}");
        let candidates = callables
            .iter()
            .filter(|candidate| candidate.path == short_path || candidate.path.ends_with(&suffix))
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }
    }

    let candidates = callables
        .iter()
        .filter(|candidate| candidate.module == caller.module && candidate.name == name)
        .collect::<Vec<_>>();
    (candidates.len() == 1)
        .then(|| candidates.into_iter().next())
        .flatten()
}

fn declaration_effects(path: &str) -> Vec<(Effect, &'static str)> {
    match path {
        "std::fs::File::open" => vec![(Effect::FileRead, "rust-std-api:File::open")],
        "std::fs::File::create" | "std::fs::File::create_new" => {
            vec![(Effect::FileWrite, "rust-std-api:File::create")]
        }
        "std::fs::OpenOptions::open" => vec![
            (Effect::FileRead, "rust-std-api:OpenOptions::open"),
            (Effect::FileWrite, "rust-std-api:OpenOptions::open"),
        ],
        _ => Vec::new(),
    }
}

fn origin_effects(callable: &Callable, callee: &str) -> Vec<Effect> {
    let identifiers = identifiers(callee);
    let operation = identifiers.last().copied().unwrap_or_default();
    let path = identifiers.join("::");
    let mut effects = BTreeSet::new();

    if path.ends_with("env_imp::getenv") {
        effects.insert(Effect::EnvironmentRead);
    }
    if path.ends_with("env_imp::setenv") || path.ends_with("env_imp::unsetenv") {
        effects.insert(Effect::EnvironmentWrite);
    }
    if path.ends_with("time::Instant::now") || path.ends_with("time::SystemTime::now") {
        effects.insert(Effect::Time);
    }
    if path.ends_with("imp::sleep") || path.ends_with("imp::sleep_until") {
        effects.extend([Effect::Block, Effect::Time]);
    }
    if path.ends_with("net_imp::TcpStream::connect")
        || path.ends_with("net_imp::TcpStream::connect_timeout")
    {
        effects.extend([Effect::Block, Effect::Network]);
    }
    if path.ends_with("imp::output") {
        effects.extend([Effect::Block, Effect::Process]);
    }
    if callable.module == "std::process" {
        match operation {
            "spawn" | "kill" | "try_wait" => {
                effects.insert(Effect::Process);
            }
            "wait" => {
                effects.extend([Effect::Block, Effect::Process]);
            }
            _ => {}
        }
    }

    if path.starts_with("fs_imp::") {
        const READS: &[&str] = &[
            "canonicalize",
            "exists",
            "metadata",
            "read",
            "read_dir",
            "read_link",
            "readlink",
            "symlink_metadata",
            "try_exists",
        ];
        const WRITES: &[&str] = &[
            "copy",
            "create_dir",
            "hard_link",
            "remove_dir",
            "remove_dir_all",
            "remove_file",
            "rename",
            "set_permissions",
            "set_times",
            "soft_link",
            "symlink",
            "write",
        ];
        if READS.contains(&operation) || operation == "copy" {
            effects.insert(Effect::FileRead);
        }
        if WRITES.contains(&operation) {
            effects.insert(Effect::FileWrite);
        }
    }
    effects.into_iter().collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EffectRelation {
    callable: u64,
    effect: u8,
}

ascent::ascent! {
    struct EffectClosure;

    /// `caller` contains a call to `callee`.
    relation calls(u64, u64);
    /// `callable` has `effect`, whether seeded or inherited from a callee.
    relation has_effect(u64, u8);

    // an effect reaches a caller through any call that reaches it
    has_effect(caller, *effect) <-- calls(caller, callee), has_effect(callee, effect);
}

/// Every effect each callable has, following calls transitively.
///
/// Seeds are the effects a callable has directly; the rule above closes them
/// over the call graph. The closure is computed from scratch, which is all any
/// caller has ever asked for -- each one builds the whole relation once from a
/// complete call graph and drops it.
fn propagate_effects(calls: &[ResolvedCall], seeds: &[EffectSeed]) -> BTreeSet<EffectRelation> {
    let mut closure = EffectClosure {
        calls: calls
            .iter()
            .map(|call| (call.caller, call.callee))
            .collect(),
        has_effect: seeds
            .iter()
            .map(|seed| (seed.callable, encode_effect(seed.effect)))
            .collect(),
        ..EffectClosure::default()
    };
    closure.run();
    closure
        .has_effect
        .into_iter()
        .map(|(callable, effect)| EffectRelation { callable, effect })
        .collect()
}

fn encode_effect(effect: Effect) -> u8 {
    match effect {
        Effect::Allocate => 0,
        Effect::Block => 1,
        Effect::EnvironmentRead => 2,
        Effect::EnvironmentWrite => 3,
        Effect::FileRead => 4,
        Effect::FileWrite => 5,
        Effect::Network => 6,
        Effect::Process => 7,
        Effect::Random => 8,
        Effect::Time => 9,
        Effect::Unsafe => 10,
    }
}

fn decode_effect(effect: u8) -> Effect {
    match effect {
        0 => Effect::Allocate,
        1 => Effect::Block,
        2 => Effect::EnvironmentRead,
        3 => Effect::EnvironmentWrite,
        4 => Effect::FileRead,
        5 => Effect::FileWrite,
        6 => Effect::Network,
        7 => Effect::Process,
        8 => Effect::Random,
        9 => Effect::Time,
        10 => Effect::Unsafe,
        _ => unreachable!("effect code was produced by encode_effect"),
    }
}

fn source_span(path: &Path, node: Node<'_>) -> Result<SourceSpan> {
    Ok(SourceSpan {
        path: path.to_path_buf(),
        start_byte: Some(
            u64::try_from(node.start_byte())
                .map_err(|_| Error::SourceTooLarge { path: path.into() })?,
        ),
        end_byte: Some(
            u64::try_from(node.end_byte())
                .map_err(|_| Error::SourceTooLarge { path: path.into() })?,
        ),
        start_line: u32::try_from(node.start_position().row + 1)
            .map_err(|_| Error::SourceTooLarge { path: path.into() })?,
        end_line: u32::try_from(node.end_position().row + 1)
            .map_err(|_| Error::SourceTooLarge { path: path.into() })?,
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

fn trace_derivation(
    trace: &EffectTrace,
    inputs: &BTreeMap<PathBuf, InputEvidence>,
) -> FactDerivation {
    let mut paths = BTreeSet::from([trace.callable_span.path.clone()]);
    paths.extend(trace.path.iter().map(|edge| edge.call.path.clone()));
    FactDerivation {
        analyzer: "infact-rust-effects".to_owned(),
        analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
        inputs: paths
            .into_iter()
            .filter_map(|path| inputs.get(&path).cloned())
            .collect(),
    }
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok() // straitjacket-allow:error-discard — a node whose bytes are not UTF-8 has no text
}

fn normalize_callee(callee: &str) -> String {
    callee
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn simple_type_name(value: &str) -> String {
    let base = value.split('<').next().unwrap_or(value);
    identifiers(base)
        .last()
        .copied()
        .unwrap_or(value)
        .to_owned()
}

fn trailing_identifier(value: &str) -> Option<&str> {
    identifiers(value).last().copied()
}

fn identifiers(value: &str) -> Vec<&str> {
    value
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|part| {
            !part.is_empty()
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        })
        .collect()
}

fn is_public_header(header: &str) -> bool {
    header
        .split_whitespace()
        .next()
        .is_some_and(|first| first == "pub")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_explicit_origins() {
        let callable = Callable {
            id: 0,
            path: "std::env::_var_os".to_owned(),
            name: "_var_os".to_owned(),
            module: "std::env".to_owned(),
            parent: None,
            is_public: false,
            is_unsafe: false,
            span: SourceSpan {
                path: "std/src/env.rs".into(),
                start_byte: Some(0),
                end_byte: Some(1),
                start_line: 1,
                end_line: 1,
                start_column: None,
                end_column: None,
            },
            syntax_calls: Vec::new(),
            allocating_macros: Vec::new(),
        };
        assert_eq!(
            origin_effects(&callable, "env_imp::getenv"),
            vec![Effect::EnvironmentRead]
        );
        assert_eq!(
            origin_effects(&callable, "fs_imp::copy"),
            vec![Effect::FileRead, Effect::FileWrite]
        );
        assert_eq!(
            origin_effects(&callable, "imp::sleep"),
            vec![Effect::Block, Effect::Time]
        );
    }

    #[test]
    fn distinguishes_constructors_from_calls() {
        assert!(is_constructor("Ok"));
        assert!(is_constructor("Poll::Ready"));
        assert!(is_constructor("SystemTime"));
        assert!(!is_constructor("Vec::new"));
        assert!(!is_constructor("value.map"));
    }

    #[test]
    fn propagates_effects_across_calls() {
        let span = SourceSpan {
            path: "std/src/fs.rs".into(),
            start_byte: Some(0),
            end_byte: Some(1),
            start_line: 1,
            end_line: 1,
            start_column: None,
            end_column: None,
        };
        let calls = vec![
            ResolvedCall {
                caller: 0,
                callee: 1,
                span: span.clone(),
            },
            ResolvedCall {
                caller: 1,
                callee: 2,
                span: span.clone(),
            },
        ];
        let seeds = vec![EffectSeed {
            callable: 2,
            effect: Effect::FileRead,
            origin: "fs_imp::read".to_owned(),
            span,
        }];
        let effects = propagate_effects(&calls, &seeds);
        assert!(effects.contains(&EffectRelation {
            callable: 0,
            effect: encode_effect(Effect::FileRead),
        }));
    }

    #[test]
    fn derives_repository_effect_traces_from_external_catalogs() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "fn adapter() { let _ = std::fs::read(\"input\"); }\nfn service() { adapter(); }\nfn handler() { service(); }\n",
        )
        .unwrap();
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let discovery =
            ParserCatalog::discover([crate_root.join("../../../entl/parser-packs/rust")]);
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        let catalog = CallEffectCatalog {
            schema: CALL_EFFECT_CATALOG_SCHEMA,
            language: "rust".to_owned(),
            version: "test".to_owned(),
            calls: vec![CallEffects {
                path: "std::fs::read".to_owned(),
                effects: vec![Effect::FileRead],
                evidence: Vec::new(),
            }],
        };

        let report =
            analyze_repository_effects(root.path(), &discovery.catalog, &[catalog]).unwrap();
        let traces = report
            .effects
            .iter()
            .map(|fact| (fact.value.callable.as_str(), fact.value.path.len()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(traces["src::adapter"], 1);
        assert_eq!(traces["src::service"], 2);
        assert_eq!(traces["src::handler"], 3);
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    }

    #[test]
    fn derives_from_a_small_std_shaped_source_tree() {
        let root = tempfile::tempdir().unwrap();
        for module in MODULES {
            let path = root.path().join(module.source);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let source = match module.module {
                "std::env" => {
                    "pub fn var() { helper() }\nfn helper() { env_imp::getenv(\"X\"); }\n"
                }
                "std::fs" => {
                    "pub struct File;\nimpl File { pub fn open() {} }\npub fn read() { fn inner() { File::open(); } inner(); }\n"
                }
                "std::net" => {
                    "pub struct TcpStream;\nimpl TcpStream { pub fn connect() { net_imp::TcpStream::connect(()); } }\n"
                }
                "std::process" => {
                    "pub struct Command;\nimpl Command { pub fn output() { imp::output(()); } }\n"
                }
                "std::thread" => "pub fn sleep() { imp::sleep(()); }\n",
                "std::time" => {
                    "pub struct SystemTime;\nimpl SystemTime { pub fn now() { time::SystemTime::now(); } }\n"
                }
                _ => unreachable!(),
            };
            std::fs::write(path, source).unwrap();
        }
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let discovery =
            ParserCatalog::discover([crate_root.join("../../../entl/parser-packs/rust")]);
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        let report = derive_std_effects(root.path(), "test", &discovery.catalog).unwrap();
        assert_eq!(report.calls.total, report.calls.accounted());
        assert!(report.calls.linked_internal > 0);
        assert!(report.calls.known_effect_origins > 0);
        let effects = report
            .catalog
            .calls
            .iter()
            .map(|call| (call.path.as_str(), call.effects.as_slice()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            effects["std::env::var"],
            [Effect::EnvironmentRead].as_slice()
        );
        assert_eq!(effects["std::fs::read"], [Effect::FileRead].as_slice());
        assert_eq!(
            effects["std::thread::sleep"],
            [Effect::Block, Effect::Time].as_slice()
        );
        assert!(
            report
                .catalog
                .calls
                .iter()
                .all(|call| !call.evidence.is_empty())
        );

        let artifact = tempfile::tempdir().unwrap();
        let layout = artifact.path().join("oci");
        let built = build_std_fact_pack(RustStdFactPackRequest {
            library_root: root.path(),
            version: "test",
            compiler_commit: Some("fixture-commit".to_owned()),
            revision: 1,
            parsers: &discovery.catalog,
            output: &layout,
        })
        .unwrap();
        assert_eq!(built.manifest.name, "rust-core");
        assert_eq!(built.manifest.sources[0].sha256, report.source_sha256);
        let cache = infact_fact_pack::FactPackCache::open(artifact.path().join("cache")).unwrap();
        let cached = cache.import_oci_layout(layout).unwrap();
        assert_eq!(cached.manifest, built.manifest);
        assert_eq!(cached.manifest_digest, built.layout.manifest_digest);
    }
}

#[cfg(test)]
mod ordering {
    use super::*;

    /// `sort()` replaced a hand-written `path, line, message` comparator. The
    /// derive is only equivalent while the fields stay in that declaration
    /// order, and nothing else would notice if one moved.
    #[test]
    fn diagnostics_order_by_path_then_line_then_message() {
        let make = |path: &str, line: u32, message: &str| RepositoryEffectDiagnostic {
            path: PathBuf::from(path),
            line,
            message: message.to_owned(),
        };
        let mut diagnostics = vec![
            make("b.rs", 1, "a"),
            make("a.rs", 9, "a"),
            make("a.rs", 2, "z"),
            make("a.rs", 2, "a"),
        ];
        let expected = vec![
            make("a.rs", 2, "a"),
            make("a.rs", 2, "z"),
            make("a.rs", 9, "a"),
            make("b.rs", 1, "a"),
        ];
        diagnostics.sort();
        assert_eq!(diagnostics, expected);
    }
}
