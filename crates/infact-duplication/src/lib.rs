//! Derived facts about exact and similar syntax-token duplication.

mod engine;
mod error;
mod token;

use entl_tree_sitter::{ParserCatalog, parse_repository};
pub use error::{Error, Result};
use infact_core::{ExactTokenClone, Fact, NearTokenClone};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use engine::{ExactEngine, NearEngine};
use token::{TokenizedFile, tokenize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ExactConfig {
    pub min_tokens: u32,
    pub min_lines: u32,
}

impl Default for ExactConfig {
    fn default() -> Self {
        Self {
            min_tokens: 50,
            min_lines: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NearConfig {
    pub min_tokens: u32,
    pub min_lines: u32,
    pub normalize_identifiers: bool,
    pub normalize_literals: bool,
    pub max_changed_percent: u8,
}

impl Default for NearConfig {
    fn default() -> Self {
        Self {
            min_tokens: 80,
            min_lines: 8,
            normalize_identifiers: true,
            normalize_literals: true,
            max_changed_percent: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisReport<T> {
    pub files_parsed: usize,
    pub clones: Vec<Fact<T>>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

pub fn analyze_repository(
    root: impl AsRef<Path>,
    parser_pack: impl AsRef<Path>,
    config: ExactConfig,
) -> Result<AnalysisReport<ExactTokenClone>> {
    let discovery = ParserCatalog::discover([parser_pack.as_ref().to_path_buf()]);
    if !discovery.errors.is_empty() {
        return Err(Error::ParserCatalog(
            discovery
                .errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    analyze_repository_with_catalog(root, &discovery.catalog, config)
}

pub fn analyze_repository_with_catalog(
    root: impl AsRef<Path>,
    catalog: &ParserCatalog,
    config: ExactConfig,
) -> Result<AnalysisReport<ExactTokenClone>> {
    let tokenized = tokenize_repository(root, catalog)?;
    let mut engine = ExactEngine::new(config)?;
    for file in tokenized.files {
        engine.replace(file)?;
    }
    Ok(AnalysisReport {
        files_parsed: tokenized.files_parsed,
        clones: engine.facts(),
        diagnostics: tokenized.diagnostics,
    })
}

pub fn analyze_repository_near(
    root: impl AsRef<Path>,
    parser_pack: impl AsRef<Path>,
    config: NearConfig,
) -> Result<AnalysisReport<NearTokenClone>> {
    let discovery = ParserCatalog::discover([parser_pack.as_ref().to_path_buf()]);
    if !discovery.errors.is_empty() {
        return Err(Error::ParserCatalog(
            discovery
                .errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    analyze_repository_near_with_catalog(root, &discovery.catalog, config)
}

pub fn analyze_repository_near_with_catalog(
    root: impl AsRef<Path>,
    catalog: &ParserCatalog,
    config: NearConfig,
) -> Result<AnalysisReport<NearTokenClone>> {
    let tokenized = tokenize_repository(root, catalog)?;
    let mut engine = NearEngine::new(config)?;
    for file in tokenized.files {
        engine.replace(file)?;
    }
    Ok(AnalysisReport {
        files_parsed: tokenized.files_parsed,
        clones: engine.facts(),
        diagnostics: tokenized.diagnostics,
    })
}

struct TokenizedRepository {
    files: Vec<TokenizedFile>,
    files_parsed: usize,
    diagnostics: Vec<AnalysisDiagnostic>,
}

fn tokenize_repository(
    root: impl AsRef<Path>,
    catalog: &ParserCatalog,
) -> Result<TokenizedRepository> {
    let parsed = parse_repository(root, catalog)?;
    let diagnostics = parsed
        .diagnostics
        .into_iter()
        .map(|diagnostic| AnalysisDiagnostic {
            path: diagnostic.path,
            message: diagnostic.message,
        })
        .collect::<Vec<_>>();
    let files_parsed = parsed.files.len();
    let files = parsed
        .files
        .into_iter()
        .map(tokenize)
        .collect::<Result<Vec<_>>>()?;

    Ok(TokenizedRepository {
        files,
        files_parsed,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use entl_tree_sitter::{ParserPack, ParserRuntime};

    use super::*;

    fn pack() -> Arc<ParserPack> {
        Arc::new(
            ParserPack::load(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust"),
            )
            .unwrap(),
        )
    }

    fn tokenized(pack: &Arc<ParserPack>, path: &str, source: &str) -> token::TokenizedFile {
        let parsed = ParserRuntime::new()
            .unwrap()
            .load(pack.clone())
            .unwrap()
            .parse(path, Arc::<[u8]>::from(source.as_bytes()))
            .unwrap();
        token::tokenize(parsed).unwrap()
    }

    #[test]
    fn replacing_a_file_retracts_stale_matches() {
        let pack = pack();
        let config = ExactConfig {
            min_tokens: 8,
            min_lines: 2,
        };
        let source = "fn one() {\n    let value = 1;\n    println!(\"{}\", value);\n}";
        let changed = "fn two() {\n    let other = 2;\n    drop(other);\n}";
        let mut incremental = engine::ExactEngine::new(config).unwrap();
        incremental
            .replace(tokenized(&pack, "a.rs", source))
            .unwrap();
        incremental
            .replace(tokenized(&pack, "b.rs", source))
            .unwrap();
        assert!(!incremental.facts().is_empty());

        incremental
            .replace(tokenized(&pack, "b.rs", changed))
            .unwrap();
        let incremental_facts = incremental.facts();

        let mut fresh = engine::ExactEngine::new(config).unwrap();
        fresh.replace(tokenized(&pack, "a.rs", source)).unwrap();
        fresh.replace(tokenized(&pack, "b.rs", changed)).unwrap();
        assert_eq!(incremental_facts, fresh.facts());
        assert!(incremental_facts.is_empty());
    }

    #[test]
    fn replacing_a_file_retracts_stale_near_matches() {
        let pack = pack();
        let config = NearConfig {
            min_tokens: 8,
            min_lines: 2,
            normalize_identifiers: true,
            normalize_literals: true,
            max_changed_percent: 100,
        };
        let first = "fn total(values: &[u32]) -> u32 {\n let mut sum = 0;\n for value in values { sum += value; }\n sum\n}";
        let renamed = "fn count(items: &[u32]) -> u32 {\n let mut result = 1;\n for item in items { result += item; }\n result\n}";
        let changed = "fn count(items: &[u32]) -> u32 {\n items.len() as u32\n}";
        let mut incremental = engine::NearEngine::new(config).unwrap();
        incremental
            .replace(tokenized(&pack, "a.rs", first))
            .unwrap();
        incremental
            .replace(tokenized(&pack, "b.rs", renamed))
            .unwrap();
        assert!(!incremental.facts().is_empty());

        incremental
            .replace(tokenized(&pack, "b.rs", changed))
            .unwrap();
        let incremental_facts = incremental.facts();

        let mut fresh = engine::NearEngine::new(config).unwrap();
        fresh.replace(tokenized(&pack, "a.rs", first)).unwrap();
        fresh.replace(tokenized(&pack, "b.rs", changed)).unwrap();
        assert_eq!(incremental_facts, fresh.facts());
        assert!(incremental_facts.is_empty());
    }
}
