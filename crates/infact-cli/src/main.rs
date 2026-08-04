use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use entl_codebase::observe_rust_compiler;
use entl_tree_sitter::ParserCatalog;
use infact_catalog::{CatalogRequest, build_catalog};
use infact_core::{DerivedLibraryBehavior, DerivedMacroBehavior, ExternalCatalog, LibraryTarget};
use infact_duplication::{
    ExactConfig, NearConfig, analyze_repository_near_with_catalog, analyze_repository_with_catalog,
};
use infact_fact_pack::{
    CachedFactPack, FactPackCache, FactPackLock, FactPackManifest, build_oci_layout, sha256,
    source_tree_sha256,
};
use infact_fact_registry::{FactPackRegistry, FactPackRegistryAuth};
use infact_rust_behaviors::{
    LibraryPackRequest, MacroDerivationRequest, analyze_repository as analyze_rust_behaviors,
    behavior_file_name, build_library_pack, derive_behavior, derive_library, derive_macro_behavior,
    registry_sources,
};
use infact_rust_effects::{RustStdFactPackRequest, build_std_fact_pack, derive_std_effects};
use serde::Deserialize;

/// Which frontend reads a library's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BehaviorLanguage {
    Rust,
    Typescript,
}

/// What every frontend hands back, so the command that writes a pack does not
/// have to know which one produced it.
struct DerivedAnyLibrary {
    catalog: ExternalCatalog,
    behaviors: Vec<DerivedLibraryBehavior>,
    unparsed: Vec<String>,
    skipped: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Parser)]
#[command(name = "infact")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build a compact external API catalog from rustdoc JSON.
    Catalog {
        rustdoc_json: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        output: PathBuf,
    },

    /// Author derived library behaviors.
    Behavior {
        #[command(subcommand)]
        command: BehaviorCommand,
    },

    /// Derive effect catalogs from language and library sources.
    Effects {
        #[command(subcommand)]
        command: EffectsCommand,
    },

    /// Author and inspect fact packs.
    Facts {
        #[command(subcommand)]
        command: FactsCommand,
    },

    /// Derive syntax-token clone facts.
    Duplication {
        #[arg(default_value = ".")]
        root: PathBuf,

        /// TOML configuration. Defaults to ROOT/infact.toml when present.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Additional parser-pack directory or directory of packs.
        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        #[arg(long)]
        min_tokens: Option<u32>,

        #[arg(long)]
        min_lines: Option<u32>,

        /// Exact copies or consistently renamed near copies.
        #[arg(long, value_enum, default_value_t = DuplicationKind::Exact)]
        kind: DuplicationKind,

        #[arg(long)]
        max_changed_percent: Option<u8>,

        #[arg(long)]
        jsonl: bool,
    },

    /// Derive Rust library-behavior match facts.
    Behaviors {
        #[arg(default_value = ".")]
        root: PathBuf,

        #[arg(long)]
        config: Option<PathBuf>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        #[arg(long = "catalog-path")]
        catalog_paths: Vec<PathBuf>,

        #[arg(long = "behavior-path")]
        behavior_paths: Vec<PathBuf>,

        #[arg(long = "macro-behavior-path")]
        macro_behavior_paths: Vec<PathBuf>,

        #[arg(long)]
        jsonl: bool,
    },
}

#[derive(Debug, Subcommand)]
enum FactsCommand {
    /// Build a verified OCI fact pack for a supported subject.
    Build {
        #[arg(long)]
        ecosystem: String,

        #[arg(long)]
        package: String,

        #[arg(long)]
        version: String,

        #[arg(long)]
        repository: PathBuf,

        #[arg(long)]
        output: PathBuf,

        #[arg(long, default_value_t = 1)]
        revision: u32,

        #[arg(long)]
        config: Option<PathBuf>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,
    },

    /// Hash a file or source tree using the fact-pack digest convention.
    Hash { path: PathBuf },

    /// Validate a fact-pack manifest and all declared content digests.
    Validate { manifest: PathBuf },

    /// Package a validated fact pack as a local OCI image layout.
    Package {
        manifest: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },

    /// Manage the verified local fact-pack cache.
    Cache {
        #[command(subcommand)]
        command: FactsCacheCommand,
    },

    /// Inspect and verify an immutable fact-pack lock.
    Lock {
        #[command(subcommand)]
        command: FactsLockCommand,
    },
}

#[derive(Debug, Subcommand)]
enum EffectsCommand {
    /// Derive a trial effect catalog from selected Rust standard-library modules.
    RustStd {
        /// Repository used to select the active Rust toolchain.
        #[arg(default_value = ".")]
        repository: PathBuf,

        /// Rust checkout `library` directory. Defaults to the active toolchain source.
        #[arg(long)]
        source: Option<PathBuf>,

        /// Rust release. Defaults to the active compiler release.
        #[arg(long)]
        version: Option<String>,

        #[arg(long)]
        config: Option<PathBuf>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum FactsCacheCommand {
    /// Verify and import a local OCI image layout.
    Import {
        layout: PathBuf,
        #[arg(long)]
        cache: PathBuf,
    },

    /// List fact packs installed in the local cache.
    List {
        #[arg(long)]
        cache: PathBuf,
    },

    /// Pull an OCI artifact, verify it, and optionally update a lock.
    Pull {
        reference: String,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        expected_digest: Option<String>,
        #[arg(long)]
        lock: Option<PathBuf>,
        #[arg(long)]
        username: Option<String>,
        /// Environment variable containing the registry password.
        #[arg(long)]
        password_env: Option<String>,
        /// Environment variable containing a registry bearer token.
        #[arg(long)]
        bearer_token_env: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum FactsLockCommand {
    /// Add an installed manifest digest to the lock.
    Add {
        #[arg(long)]
        lock: PathBuf,
        #[arg(long)]
        cache: PathBuf,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        origin: Option<String>,
    },

    /// Verify that every locked artifact is installed and unchanged.
    Verify {
        #[arg(long)]
        lock: PathBuf,
        #[arg(long)]
        cache: PathBuf,
    },

    /// List immutable artifact selections without accessing the cache.
    List {
        #[arg(long)]
        lock: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum BehaviorCommand {
    /// Print the normalized form of functions in a file.
    Show {
        source: PathBuf,

        /// Only functions whose name contains this.
        #[arg(long)]
        function: Option<String>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Derive a whole library's behaviors from its source.
    Library {
        source_root: PathBuf,

        /// Which language's frontend reads the source.
        ///
        /// The laws and the walk are shared; only how a function, a type and a
        /// public name are spelled differs.
        #[arg(long, default_value = "rust")]
        language: BehaviorLanguage,

        #[arg(long)]
        package: String,

        #[arg(long)]
        version: String,

        #[arg(long)]
        config: Option<PathBuf>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        /// Directory to write the catalog and behaviors into.
        #[arg(long)]
        output: PathBuf,

        /// Accept a pack built from source the parser could not fully read.
        #[arg(long)]
        allow_unread: bool,

        /// Report why the callables that yielded no behavior yielded none.
        #[arg(long)]
        explain: bool,
    },

    /// Derive a normalized behavior from a library implementation.
    Derive {
        source_root: PathBuf,

        #[arg(long)]
        callable: String,

        #[arg(long)]
        config: Option<PathBuf>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        #[arg(long = "catalog-path")]
        catalog_paths: Vec<PathBuf>,

        #[arg(long)]
        output: PathBuf,
    },

    /// Derive normalized behavior from a proc-macro expansion probe.
    DeriveMacro {
        probe_root: PathBuf,

        #[arg(long)]
        type_name: String,

        #[arg(long)]
        macro_package: String,

        #[arg(long)]
        macro_version: String,

        #[arg(long)]
        derive_path: String,

        #[arg(long)]
        config: Option<PathBuf>,

        #[arg(long = "parser-path")]
        parser_paths: Vec<PathBuf>,

        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DuplicationKind {
    Exact,
    Near,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    parsers: ParserConfig,
    #[serde(default)]
    catalogs: CatalogConfig,
    #[serde(default)]
    behaviors: BehaviorConfig,
    #[serde(default, rename = "macro-behaviors")]
    macro_behaviors: BehaviorConfig,
    #[serde(default)]
    duplication: DuplicationConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorConfig {
    #[serde(default, rename = "search-paths")]
    search_paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogConfig {
    #[serde(default, rename = "search-paths")]
    search_paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParserConfig {
    #[serde(default, rename = "search-paths")]
    search_paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicationConfig {
    #[serde(default)]
    exact: ExactConfigFile,
    #[serde(default)]
    near: NearConfigFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NearConfigFile {
    #[serde(rename = "min-tokens")]
    min_tokens: Option<u32>,
    #[serde(rename = "min-lines")]
    min_lines: Option<u32>,
    #[serde(rename = "normalize-identifiers")]
    normalize_identifiers: Option<bool>,
    #[serde(rename = "normalize-literals")]
    normalize_literals: Option<bool>,
    #[serde(rename = "max-changed-percent")]
    max_changed_percent: Option<u8>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactConfigFile {
    #[serde(rename = "min-tokens")]
    min_tokens: Option<u32>,
    #[serde(rename = "min-lines")]
    min_lines: Option<u32>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Catalog {
            rustdoc_json,
            package,
            version,
            output,
        } => {
            let source = std::fs::read(rustdoc_json)?;
            let catalog = build_catalog(CatalogRequest {
                package: &package,
                version: &version,
                rustdoc_json: &source,
            })?;
            std::fs::write(output, serde_json::to_vec_pretty(&catalog)?)?;
        }
        Command::Effects {
            command:
                EffectsCommand::RustStd {
                    repository,
                    source,
                    version,
                    config,
                    mut parser_paths,
                    output,
                },
        } => {
            let (file_config, base) = load_config(&repository, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            let compiler = if source.is_none() || version.is_none() {
                Some(observe_rust_compiler(&repository)?)
            } else {
                None
            };
            let source = match source {
                Some(source) => source,
                None => compiler
                    .as_ref()
                    .and_then(|compiler| compiler.standard_library_source.clone())
                    .ok_or(
                        "the active Rust toolchain has no standard-library source; install rust-src or pass --source",
                    )?,
            };
            let version = version.unwrap_or_else(|| {
                compiler
                    .as_ref()
                    .expect("compiler observed when version is absent")
                    .version
                    .clone()
            });
            let report = derive_std_effects(source, version, &parsers)?;
            std::fs::write(&output, serde_json::to_vec_pretty(&report.catalog)?)?;
            println!(
                "{} effectful public APIs from {} public callables; {} direct seeds",
                report.catalog.calls.len(),
                report.public_callables,
                report.direct_seeds,
            );
            println!("calls: {} total", report.calls.total);
            println!(
                "  {:>4} linked inside selected modules",
                report.calls.linked_internal
            );
            println!(
                "  {:>4} known effect origins",
                report.calls.known_effect_origins
            );
            println!("  {:>4} constructors", report.calls.constructors);
            println!(
                "  {:>4} outside selected modules",
                report.calls.outside_selected_corpus
            );
            println!(
                "  {:>4} dynamic or ambiguous methods",
                report.calls.dynamic_or_ambiguous
            );
            println!("  {:>4} unknown", report.calls.unknown);
            println!("  {:>4} total unlinked", report.calls.unlinked());
        }
        Command::Facts {
            command:
                FactsCommand::Build {
                    ecosystem,
                    package,
                    version,
                    repository,
                    output,
                    revision,
                    config,
                    mut parser_paths,
                },
        } => {
            if ecosystem != "cargo" {
                return Err(format!(
                    "Infact-pack authoring is not implemented for {ecosystem}; supported ecosystem is cargo"
                )
                .into());
            }
            // Any package other than the language itself is a library, and a
            // library's behaviors come from its source, which Cargo has already
            // unpacked locally.
            if package != "core" {
                let (file_config, base) = load_config(&repository, config.as_deref())?;
                parser_paths.extend(
                    file_config
                        .parsers
                        .search_paths
                        .into_iter()
                        .map(|path| resolve(&base, path)),
                );
                let parsers = parser_catalog(parser_paths)?;
                let source_root = registry_sources(&package, &version)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        format!(
                            "no unpacked source for {package} {version}; run `cargo fetch` first"
                        )
                    })?;
                let built = build_library_pack(LibraryPackRequest {
                    source_root: &source_root,
                    package: &package,
                    version: &version,
                    revision,
                    parsers: &parsers,
                    output: &output,
                })?;
                println!(
                    "{} {} revision {}  {}  {} callables  {} behaviors",
                    built.manifest.name,
                    built.manifest.subject.version,
                    built.manifest.revision,
                    built.layout.manifest_digest,
                    built.callables,
                    built.behaviors
                );
                return Ok(());
            }
            let (file_config, base) = load_config(&repository, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            let compiler = observe_rust_compiler(&repository)?;
            if compiler.version != version {
                return Err(format!(
                    "requested rust-core {version}, but the active compiler is {}",
                    compiler.version
                )
                .into());
            }
            let source = compiler.standard_library_source.as_ref().ok_or(
                "the active Rust toolchain has no standard-library source; install rust-src",
            )?;
            let built = build_std_fact_pack(RustStdFactPackRequest {
                library_root: source,
                version: &version,
                compiler_commit: compiler.commit,
                revision,
                parsers: &parsers,
                output: &output,
            })?;
            println!(
                "{} {} revision {}  {}  {} public effect summaries",
                built.manifest.name,
                built.manifest.subject.version,
                built.manifest.revision,
                built.layout.manifest_digest,
                built.report.catalog.calls.len(),
            );
        }
        Command::Facts {
            command: FactsCommand::Hash { path },
        } => {
            let digest = if path.is_dir() {
                source_tree_sha256(&path)?
            } else {
                sha256(&std::fs::read(path)?)
            };
            println!("{digest}");
        }
        Command::Facts {
            command: FactsCommand::Validate { manifest },
        } => {
            let source = std::fs::read_to_string(&manifest)?;
            let pack = FactPackManifest::parse(&source)?;
            let root = manifest.parent().unwrap_or_else(|| Path::new("."));
            pack.verify_contents(root)?;
            println!(
                "{} {} revision {}  {}  {} content blob(s)",
                pack.name,
                pack.subject.version,
                pack.revision,
                pack.manifest_sha256()?,
                pack.contents.len()
            );
        }
        Command::Facts {
            command: FactsCommand::Package { manifest, output },
        } => {
            let source = std::fs::read_to_string(&manifest)?;
            let pack = FactPackManifest::parse(&source)?;
            let root = manifest.parent().unwrap_or_else(|| Path::new("."));
            let layout = build_oci_layout(&pack, root, &output)?;
            println!(
                "{} {} revision {}  {}",
                pack.name, pack.subject.version, pack.revision, layout.manifest_digest
            );
        }
        Command::Facts {
            command:
                FactsCommand::Cache {
                    command: FactsCacheCommand::Import { layout, cache },
                },
        } => {
            let pack = FactPackCache::open(cache)?.import_oci_layout(layout)?;
            println!(
                "{} {} revision {}  {}",
                pack.manifest.name,
                pack.manifest.subject.version,
                pack.manifest.revision,
                pack.manifest_digest
            );
        }
        Command::Facts {
            command:
                FactsCommand::Cache {
                    command: FactsCacheCommand::List { cache },
                },
        } => {
            for pack in FactPackCache::open(cache)?.list()? {
                print_cached_pack(&pack);
            }
        }
        Command::Facts {
            command:
                FactsCommand::Cache {
                    command:
                        FactsCacheCommand::Pull {
                            reference,
                            cache,
                            expected_digest,
                            lock,
                            username,
                            password_env,
                            bearer_token_env,
                        },
                },
        } => {
            let auth = registry_auth(username, password_env, bearer_token_env)?;
            let cache = FactPackCache::open(cache)?;
            let runtime = tokio::runtime::Runtime::new()?;
            let pack = runtime.block_on(FactPackRegistry::default().pull(
                &reference,
                &auth,
                expected_digest.as_deref(),
                &cache,
            ))?;
            if let Some(path) = lock {
                let mut locked = FactPackLock::read_or_default(&path)?;
                locked.insert(&pack, Some(reference))?;
                locked.write(&path)?;
            }
            print_cached_pack(&pack);
        }
        Command::Facts {
            command:
                FactsCommand::Lock {
                    command:
                        FactsLockCommand::Add {
                            lock,
                            cache,
                            digest,
                            origin,
                        },
                },
        } => {
            let cache = FactPackCache::open(cache)?;
            let pack = cache.load(&digest)?;
            let mut locked = FactPackLock::read_or_default(&lock)?;
            locked.insert(&pack, origin)?;
            locked.write(&lock)?;
            print_cached_pack(&pack);
        }
        Command::Facts {
            command:
                FactsCommand::Lock {
                    command: FactsLockCommand::Verify { lock, cache },
                },
        } => {
            let cache = FactPackCache::open(cache)?;
            for pack in FactPackLock::read(&lock)?.verify(&cache)? {
                print_cached_pack(&pack);
            }
        }
        Command::Facts {
            command:
                FactsCommand::Lock {
                    command: FactsLockCommand::List { lock },
                },
        } => {
            for entry in FactPackLock::read(&lock)?.packs {
                println!(
                    "{} {} revision {}  {}{}",
                    entry.manifest.name,
                    entry.manifest.subject.version,
                    entry.manifest.revision,
                    entry.manifest_digest,
                    entry
                        .origin
                        .map(|origin| format!("  {origin}"))
                        .unwrap_or_default()
                );
            }
        }
        Command::Behavior {
            command:
                BehaviorCommand::Show {
                    source,
                    function,
                    mut parser_paths,
                    config,
                },
        } => {
            let root = source.parent().unwrap_or(Path::new(".")).to_path_buf();
            let (file_config, base) = load_config(&root, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            // a single file is inspected by walking its directory and keeping it
            let (walk, only) = if source.is_dir() {
                (source.clone(), None)
            } else {
                (root.clone(), source.file_name().map(|name| name.to_owned()))
            };
            let parsed = entl_tree_sitter::parse_repository(&walk, &parsers)?;
            for diagnostic in &parsed.diagnostics {
                eprintln!(
                    "unread {}: {}",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            for file in &parsed.files {
                if only
                    .as_ref()
                    .is_some_and(|name| file.path.file_name() != Some(name.as_os_str()))
                {
                    continue;
                }
                for normalized in infact_rust_normalize::normalize_file(file) {
                    if function
                        .as_ref()
                        .is_some_and(|wanted| !normalized.name.contains(wanted.as_str()))
                    {
                        continue;
                    }
                    let simplified = normalized.form.simplify();
                    println!(
                        "{}:{}  {}\n  as written: {}\n  simplified: {}",
                        file.path.display(),
                        normalized.start_line,
                        normalized.name,
                        normalized.form,
                        simplified
                    );
                }
            }
        }
        Command::Behavior {
            command:
                BehaviorCommand::Library {
                    source_root,
                    language,
                    package,
                    version,
                    config,
                    mut parser_paths,
                    output,
                    allow_unread,
                    explain,
                },
        } => {
            let (file_config, base) = load_config(&source_root, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            let derived = match language {
                BehaviorLanguage::Rust => {
                    let derived = derive_library(&source_root, &parsers, &package, &version)?;
                    DerivedAnyLibrary {
                        catalog: derived.catalog,
                        behaviors: derived.behaviors,
                        unparsed: derived.unparsed,
                        skipped: derived.skipped,
                    }
                }
                BehaviorLanguage::Typescript => {
                    let derived = infact_ts_behaviors::derive_library(
                        &source_root,
                        &parsers,
                        &package,
                        &version,
                    )?;
                    // A function read in part is a hole in the pack, so it is
                    // named in the unread list as well as counted among the
                    // skips: the count says how big the hole is and the names
                    // say whether it is anywhere that matters.
                    let mut unparsed = derived.unparsed;
                    unparsed.extend(
                        derived
                            .damaged
                            .iter()
                            .map(|name| format!("{name}: the parser could not read it whole")),
                    );
                    DerivedAnyLibrary {
                        catalog: derived.catalog,
                        behaviors: derived.behaviors,
                        unparsed,
                        skipped: derived.skipped,
                    }
                }
            };
            let (catalog, behaviors) = (&derived.catalog, &derived.behaviors);

            std::fs::create_dir_all(output.join("api"))?;
            std::fs::create_dir_all(output.join("behaviors"))?;
            std::fs::write(
                output.join("api").join(format!("{package}-{version}.json")),
                serde_json::to_vec_pretty(&catalog)?,
            )?;

            for behavior in behaviors {
                std::fs::write(
                    output.join("behaviors").join(behavior_file_name(
                        &package,
                        &behavior.callable_path,
                        &version,
                    )),
                    serde_json::to_vec_pretty(behavior)?,
                )?;
            }
            println!(
                "{package} {version}  {} public callables  {} behaviors",
                catalog.callables.len(),
                behaviors.len()
            );
            if explain {
                let mut reasons = derived.skipped.iter().collect::<Vec<_>>();
                reasons.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
                for (reason, count) in reasons {
                    println!("  {count:>6}  {reason}");
                }
            }
            // Source the parser cannot read is a hole in the result, not a
            // property of the library. Reporting it quietly invites the hole to
            // be mistaken for an answer, so this fails.
            if !derived.unparsed.is_empty() && !allow_unread {
                for path in &derived.unparsed {
                    eprintln!("  unread: {path}");
                }
                return Err(format!(
                    "{} holes in what could be read, so anything they cover is missing from this pack; \
                     add a rewrite in entl-tree-sitter's dialect module, or pass --allow-unread to accept the gap",
                    derived.unparsed.len()
                )
                .into());
            }
        }
        Command::Behavior {
            command:
                BehaviorCommand::Derive {
                    source_root,
                    callable,
                    config,
                    mut parser_paths,
                    mut catalog_paths,
                    output,
                },
        } => {
            let (file_config, base) = load_config(&source_root, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            catalog_paths.extend(
                file_config
                    .catalogs
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            let catalogs = load_json_files::<ExternalCatalog>(catalog_paths, "catalog", false)?;
            let catalog = catalogs
                .iter()
                .find(|catalog| {
                    catalog
                        .callables
                        .iter()
                        .any(|candidate| candidate.path == callable)
                })
                .ok_or_else(|| format!("no external catalog contains {callable}"))?;
            let behavior = derive_behavior(source_root, &parsers, catalog, &callable)?;
            println!(
                "{}  size {}  from {}",
                behavior.callable_path,
                behavior.program.size(),
                behavior
                    .implementation
                    .iter()
                    .map(|evidence| evidence.callable_path.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            );
            std::fs::write(output, serde_json::to_vec_pretty(&behavior)?)?;
        }
        Command::Behavior {
            command:
                BehaviorCommand::DeriveMacro {
                    probe_root,
                    type_name,
                    macro_package,
                    macro_version,
                    derive_path,
                    config,
                    mut parser_paths,
                    output,
                },
        } => {
            let (file_config, base) = load_config(&probe_root, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            let probe_source = std::fs::read(probe_root.join("src/lib.rs"))?;
            let target = tempfile::tempdir()?;
            let process = std::process::Command::new("cargo")
                .arg("check")
                .arg("--offline")
                .arg("--manifest-path")
                .arg(probe_root.join("Cargo.toml"))
                .env("STRUM_DEBUG", &type_name)
                .env("CARGO_TARGET_DIR", target.path())
                .env("RUSTC_WRAPPER", "")
                .output()?;
            if !process.status.success() {
                return Err(format!(
                    "macro expansion probe failed:\n{}",
                    String::from_utf8_lossy(&process.stderr)
                )
                .into());
            }
            let behavior = derive_macro_behavior(
                &parsers,
                MacroDerivationRequest {
                    macro_package: &macro_package,
                    macro_version: &macro_version,
                    derive_path: &derive_path,
                    probe_source: &probe_source,
                    expansion: &process.stdout,
                },
            )?;
            std::fs::write(output, serde_json::to_vec_pretty(&behavior)?)?;
        }
        Command::Duplication {
            root,
            config,
            mut parser_paths,
            min_tokens,
            min_lines,
            kind,
            max_changed_percent,
            jsonl,
        } => {
            let (file_config, base) = load_config(&root, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let catalog = parser_catalog(parser_paths)?;
            match kind {
                DuplicationKind::Exact => {
                    let defaults = ExactConfig::default();
                    let report = analyze_repository_with_catalog(
                        root,
                        &catalog,
                        ExactConfig {
                            min_tokens: min_tokens
                                .or(file_config.duplication.exact.min_tokens)
                                .unwrap_or(defaults.min_tokens),
                            min_lines: min_lines
                                .or(file_config.duplication.exact.min_lines)
                                .unwrap_or(defaults.min_lines),
                        },
                    )?;
                    print_diagnostics(&report.diagnostics);
                    for fact in &report.clones {
                        if jsonl {
                            println!("{}", serde_json::to_string(fact)?);
                        } else {
                            let clone = &fact.value;
                            println!(
                                "{}:{}-{} = {}:{}-{}  {} tokens",
                                clone.left.path.display(),
                                clone.left.start_line,
                                clone.left.end_line,
                                clone.right.path.display(),
                                clone.right.start_line,
                                clone.right.end_line,
                                clone.tokens,
                            );
                        }
                    }
                }
                DuplicationKind::Near => {
                    let defaults = NearConfig::default();
                    let configured = file_config.duplication.near;
                    let report = analyze_repository_near_with_catalog(
                        root,
                        &catalog,
                        NearConfig {
                            min_tokens: min_tokens
                                .or(configured.min_tokens)
                                .unwrap_or(defaults.min_tokens),
                            min_lines: min_lines
                                .or(configured.min_lines)
                                .unwrap_or(defaults.min_lines),
                            normalize_identifiers: configured
                                .normalize_identifiers
                                .unwrap_or(defaults.normalize_identifiers),
                            normalize_literals: configured
                                .normalize_literals
                                .unwrap_or(defaults.normalize_literals),
                            max_changed_percent: max_changed_percent
                                .or(configured.max_changed_percent)
                                .unwrap_or(defaults.max_changed_percent),
                        },
                    )?;
                    print_diagnostics(&report.diagnostics);
                    for fact in &report.clones {
                        if jsonl {
                            println!("{}", serde_json::to_string(fact)?);
                        } else {
                            let clone = &fact.value;
                            println!(
                                "{}:{}-{} ~ {}:{}-{}  {} tokens, {} changed",
                                clone.left.path.display(),
                                clone.left.start_line,
                                clone.left.end_line,
                                clone.right.path.display(),
                                clone.right.start_line,
                                clone.right.end_line,
                                clone.tokens,
                                clone.changed_tokens,
                            );
                        }
                    }
                }
            }
        }
        Command::Behaviors {
            root,
            config,
            mut parser_paths,
            mut catalog_paths,
            mut behavior_paths,
            mut macro_behavior_paths,
            jsonl,
        } => {
            let (file_config, base) = load_config(&root, config.as_deref())?;
            parser_paths.extend(
                file_config
                    .parsers
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            catalog_paths.extend(
                file_config
                    .catalogs
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            behavior_paths.extend(
                file_config
                    .behaviors
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            macro_behavior_paths.extend(
                file_config
                    .macro_behaviors
                    .search_paths
                    .into_iter()
                    .map(|path| resolve(&base, path)),
            );
            let parsers = parser_catalog(parser_paths)?;
            let catalogs = load_json_files::<ExternalCatalog>(catalog_paths, "catalog", true)?;
            let behaviors =
                load_json_files::<DerivedLibraryBehavior>(behavior_paths, "behavior", false)?;
            let macro_behaviors = load_json_files::<DerivedMacroBehavior>(
                macro_behavior_paths,
                "macro behavior",
                false,
            )?;
            let report =
                analyze_rust_behaviors(root, &parsers, &catalogs, &behaviors, &macro_behaviors)?;
            for diagnostic in &report.diagnostics {
                eprintln!("{}: {}", diagnostic.path.display(), diagnostic.message);
            }
            for fact in &report.matches {
                if jsonl {
                    println!("{}", serde_json::to_string(fact)?);
                } else {
                    let behavior_match = &fact.value;
                    // When several callables share the behavior, saying only one
                    // of them would be a guess about the receiver's type.
                    let undecided = if behavior_match.alternatives.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " (or {})",
                            behavior_match
                                .alternatives
                                .iter()
                                .map(LibraryTarget::path)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    println!(
                        "{}:{}-{}  {}{undecided}",
                        behavior_match.span.path.display(),
                        behavior_match.span.start_line,
                        behavior_match.span.end_line,
                        behavior_match.target.path(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn parser_catalog(paths: Vec<PathBuf>) -> Result<ParserCatalog, Box<dyn std::error::Error>> {
    if paths.is_empty() {
        return Err(
            "no parser paths configured; set [parsers].search-paths or pass --parser-path".into(),
        );
    }
    let discovery = ParserCatalog::discover(paths);
    if !discovery.errors.is_empty() {
        return Err(discovery
            .errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ")
            .into());
    }
    Ok(discovery.catalog)
}

fn registry_auth(
    username: Option<String>,
    password_env: Option<String>,
    bearer_token_env: Option<String>,
) -> Result<FactPackRegistryAuth, Box<dyn std::error::Error>> {
    match (username, password_env, bearer_token_env) {
        (None, None, None) => Ok(FactPackRegistryAuth::Anonymous),
        (Some(username), Some(variable), None) => {
            let password = std::env::var(&variable)
                .map_err(|error| format!("reading registry password from {variable}: {error}"))?;
            if password.is_empty() {
                return Err(format!("registry password in {variable} is empty").into());
            }
            Ok(FactPackRegistryAuth::Basic { username, password })
        }
        (None, None, Some(variable)) => {
            let token = std::env::var(&variable)
                .map_err(|error| format!("reading registry token from {variable}: {error}"))?;
            if token.is_empty() {
                return Err(format!("registry token in {variable} is empty").into());
            }
            Ok(FactPackRegistryAuth::Bearer(token))
        }
        _ => Err("use --username with --password-env, or use --bearer-token-env alone".into()),
    }
}

fn print_cached_pack(pack: &CachedFactPack) {
    println!(
        "{} {} revision {}  {}",
        pack.manifest.name,
        pack.manifest.subject.version,
        pack.manifest.revision,
        pack.manifest_digest
    );
}

fn load_json_files<T: serde::de::DeserializeOwned>(
    paths: Vec<PathBuf>,
    kind: &str,
    required: bool,
) -> Result<Vec<T>, Box<dyn std::error::Error>> {
    if paths.is_empty() {
        if required {
            return Err(format!("no {kind} paths configured").into());
        }
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path);
            continue;
        }
        // A dropped entry here silently shortens the input set, so a partial
        // read would look like a smaller repository.
        let mut children = std::fs::read_dir(path)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        children.sort();
        files.extend(children);
    }
    files
        .into_iter()
        .map(|path| Ok(serde_json::from_slice(&std::fs::read(path)?)?))
        .collect()
}

fn print_diagnostics(diagnostics: &[infact_duplication::AnalysisDiagnostic]) {
    for diagnostic in diagnostics {
        eprintln!("{}: {}", diagnostic.path.display(), diagnostic.message);
    }
}

fn load_config(
    root: &Path,
    requested: Option<&Path>,
) -> Result<(Config, PathBuf), Box<dyn std::error::Error>> {
    let path = requested.map(Path::to_path_buf).or_else(|| {
        let candidate = root.join("infact.toml");
        candidate.is_file().then_some(candidate)
    });
    let Some(path) = path else {
        return Ok((Config::default(), std::env::current_dir()?));
    };
    let source = std::fs::read_to_string(&path)?;
    let config = toml::from_str(&source)?;
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok((config, base))
}

fn resolve(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
