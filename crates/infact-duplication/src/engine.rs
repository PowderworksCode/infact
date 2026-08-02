use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};

use dbsp::typed_batch::IndexedZSetReader;
use dbsp::{DBSPHandle, OrdZSet, OutputHandle, Runtime, ZSetHandle};
use feldera_macros::IsNone;
use infact_core::{
    Derivation, ExactTokenClone, Fact, InputEvidence, NearTokenClone, SourceSpan,
    TokenNormalization,
};
use rkyv::{Archive, Deserialize, Serialize};
use size_of::SizeOf;

use crate::token::{
    Normalization, SyntaxToken, TokenizedFile, changed_token_count, normalized_token_digest,
    normalized_token_identity, token_digest,
};
use crate::{Error, ExactConfig, NearConfig, Result};

#[derive(Debug, Clone, Copy)]
enum MatchMode {
    Exact,
    Near {
        normalization: Normalization,
        max_changed_percent: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CloneMatch {
    left: SourceSpan,
    right: SourceSpan,
    left_unit: SourceSpan,
    right_unit: SourceSpan,
    tokens: u32,
    changed_tokens: u32,
    signature_sha256: String,
    inputs: Vec<InputEvidence>,
}

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    SizeOf,
    Archive,
    Serialize,
    Deserialize,
    IsNone,
)]
#[archive_attr(derive(Ord, Eq, PartialEq, PartialOrd))]
#[archive(compare(PartialEq, PartialOrd))]
struct WindowRecord {
    domain: String,
    digest: String,
    file: u64,
    start_token: u32,
    end_token: u32,
    start_byte: u64,
    end_byte: u64,
    start_line: u32,
    end_line: u32,
}

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    SizeOf,
    Archive,
    Serialize,
    Deserialize,
    IsNone,
)]
#[archive_attr(derive(Ord, Eq, PartialEq, PartialOrd))]
#[archive(compare(PartialEq, PartialOrd))]
struct WindowLocation {
    file: u64,
    start_token: u32,
    end_token: u32,
    start_byte: u64,
    end_byte: u64,
    start_line: u32,
    end_line: u32,
}

impl From<&WindowRecord> for WindowLocation {
    fn from(window: &WindowRecord) -> Self {
        Self {
            file: window.file,
            start_token: window.start_token,
            end_token: window.end_token,
            start_byte: window.start_byte,
            end_byte: window.end_byte,
            start_line: window.start_line,
            end_line: window.end_line,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    SizeOf,
    Archive,
    Serialize,
    Deserialize,
    IsNone,
)]
#[archive_attr(derive(Ord, Eq, PartialEq, PartialOrd))]
#[archive(compare(PartialEq, PartialOrd))]
struct MatchSeed {
    left: WindowLocation,
    right: WindowLocation,
}

impl MatchSeed {
    fn new(first: &WindowLocation, second: &WindowLocation) -> Self {
        if (first.file, first.start_token) < (second.file, second.start_token) {
            Self {
                left: first.clone(),
                right: second.clone(),
            }
        } else {
            Self {
                left: second.clone(),
                right: first.clone(),
            }
        }
    }

    fn is_distinct_nonoverlapping(&self) -> bool {
        (self.left.file, self.left.start_token) < (self.right.file, self.right.start_token)
            && (self.left.file != self.right.file || self.left.end_token < self.right.start_token)
    }
}

struct Run {
    first: MatchSeed,
    last: MatchSeed,
}

impl Run {
    fn new(seed: MatchSeed) -> Self {
        Self {
            first: seed.clone(),
            last: seed,
        }
    }

    fn can_extend(&self, seed: &MatchSeed) -> bool {
        self.last.left.file == seed.left.file
            && self.last.right.file == seed.right.file
            && self.last.left.start_token.checked_add(1) == Some(seed.left.start_token)
            && self.last.right.start_token.checked_add(1) == Some(seed.right.start_token)
    }

    fn extend(&mut self, seed: MatchSeed) {
        self.last = seed;
    }
}

pub(crate) struct ExactEngine {
    min_tokens: u32,
    min_lines: u32,
    mode: MatchMode,
    circuit: DBSPHandle,
    input: ZSetHandle<WindowRecord>,
    output: OutputHandle<OrdZSet<MatchSeed>>,
    next_file: u64,
    file_ids: BTreeMap<PathBuf, u64>,
    windows: BTreeMap<u64, Vec<WindowRecord>>,
    files: BTreeMap<u64, TokenizedFile>,
    seeds: BTreeMap<MatchSeed, i64>,
}

impl ExactEngine {
    pub fn new(config: ExactConfig) -> Result<Self> {
        Self::with_mode(config.min_tokens, config.min_lines, MatchMode::Exact)
    }

    fn with_mode(min_tokens: u32, min_lines: u32, mode: MatchMode) -> Result<Self> {
        if min_tokens == 0 {
            return Err(Error::InvalidConfig(
                "min-tokens must be greater than zero".to_owned(),
            ));
        }
        if min_lines == 0 {
            return Err(Error::InvalidConfig(
                "min-lines must be greater than zero".to_owned(),
            ));
        }
        let (circuit, (input, output)) = Runtime::init_circuit(1, |circuit| {
            let (windows, input) = circuit.add_input_zset::<WindowRecord>();
            let indexed = windows.map_index(|window| {
                (
                    (window.domain.clone(), window.digest.clone()),
                    WindowLocation::from(window),
                )
            });
            let matches = indexed
                .join(&indexed, |_key, first, second| {
                    MatchSeed::new(first, second)
                })
                .filter(MatchSeed::is_distinct_nonoverlapping);
            Ok((input, matches.output()))
        })
        .map_err(|error| Error::Dbsp(error.to_string()))?;

        Ok(Self {
            min_tokens,
            min_lines,
            mode,
            circuit,
            input,
            output,
            next_file: 0,
            file_ids: BTreeMap::new(),
            windows: BTreeMap::new(),
            files: BTreeMap::new(),
            seeds: BTreeMap::new(),
        })
    }

    pub fn replace(&mut self, file: TokenizedFile) -> Result<()> {
        let file_id = if let Some(file_id) = self.file_ids.get(&file.path) {
            *file_id
        } else {
            let file_id = self.next_file;
            self.next_file += 1;
            self.file_ids.insert(file.path.clone(), file_id);
            file_id
        };

        if let Some(previous) = self.windows.remove(&file_id) {
            for window in previous {
                self.input.push(window, -1);
            }
        }
        let windows = build_windows(file_id, &file, self.min_tokens, self.mode);
        for window in &windows {
            self.input.push(window.clone(), 1);
        }
        self.windows.insert(file_id, windows);
        self.files.insert(file_id, file);
        self.advance()
    }

    fn advance(&mut self) -> Result<()> {
        self.circuit
            .transaction()
            .map_err(|error| Error::Dbsp(error.to_string()))?;
        for (seed, (), weight) in self.output.consolidate().iter() {
            let count = self.seeds.get(&seed).copied().unwrap_or_default() + weight;
            if count == 0 {
                self.seeds.remove(&seed);
            } else {
                self.seeds.insert(seed, count);
            }
        }
        Ok(())
    }

    pub fn facts(&self) -> Vec<Fact<ExactTokenClone>> {
        self.matches()
            .into_iter()
            .map(|matched| Fact {
                value: ExactTokenClone {
                    left: matched.left,
                    right: matched.right,
                    tokens: matched.tokens,
                    token_sha256: matched.signature_sha256,
                },
                derivation: Derivation {
                    analyzer: "duplication.exact-token".to_owned(),
                    analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
                    inputs: matched.inputs,
                },
            })
            .collect()
    }

    fn matches(&self) -> Vec<CloneMatch> {
        let mut runs = Vec::<Run>::new();
        let mut seeds = self
            .seeds
            .iter()
            .filter(|(_, weight)| **weight > 0)
            .map(|(seed, _)| seed.clone())
            .collect::<Vec<_>>();
        seeds.sort_by_key(|seed| {
            (
                seed.left.file,
                seed.right.file,
                i64::from(seed.right.start_token) - i64::from(seed.left.start_token),
                seed.left.start_token,
            )
        });
        for seed in seeds {
            if let Some(run) = runs.last_mut()
                && run.can_extend(&seed)
            {
                run.extend(seed);
            } else {
                runs.push(Run::new(seed));
            }
        }

        let mut facts = BTreeSet::new();
        for run in runs {
            if let Some(matched) = self.match_for_run(&run) {
                facts.insert(matched);
            }
        }
        maximal_matches(facts.into_iter().collect())
    }

    fn match_for_run(&self, run: &Run) -> Option<CloneMatch> {
        let left_file = self.files.get(&run.first.left.file)?;
        let right_file = self.files.get(&run.first.right.file)?;
        let left_start = run.first.left.start_token as usize;
        let right_start = run.first.right.start_token as usize;
        let token_count = run
            .last
            .left
            .end_token
            .checked_sub(run.first.left.start_token)?
            .checked_add(1)?;
        let left_end = left_start.checked_add(token_count as usize)?;
        let right_end = right_start.checked_add(token_count as usize)?;
        let left_tokens = left_file.tokens.get(left_start..left_end)?;
        let right_tokens = right_file.tokens.get(right_start..right_end)?;
        let (changed_tokens, signature_sha256) = match self.mode {
            MatchMode::Exact => {
                if !same_tokens(left_tokens, right_tokens) {
                    return None;
                }
                (0, token_digest(left_tokens))
            }
            MatchMode::Near {
                normalization,
                max_changed_percent,
            } => {
                if normalized_token_identity(left_tokens, normalization)
                    != normalized_token_identity(right_tokens, normalization)
                {
                    return None;
                }
                let changed = changed_token_count(left_tokens, right_tokens);
                if changed == 0
                    || u64::from(changed) * 100
                        > u64::from(token_count) * u64::from(max_changed_percent)
                {
                    return None;
                }
                (changed, normalized_token_digest(left_tokens, normalization))
            }
        };
        if run.first.left.file == run.first.right.file
            && ranges_overlap(left_start, left_end, right_start, right_end)
        {
            return None;
        }

        let left = span(&left_file.path, left_tokens)?;
        let right = span(&right_file.path, right_tokens)?;
        if left.line_count() < self.min_lines || right.line_count() < self.min_lines {
            return None;
        }

        let mut inputs = vec![left_file.evidence.clone(), right_file.evidence.clone()];
        inputs.sort();
        Some(CloneMatch {
            left_unit: comparison_unit(left_file, &left),
            right_unit: comparison_unit(right_file, &right),
            left,
            right,
            tokens: token_count,
            changed_tokens,
            signature_sha256,
            inputs,
        })
    }
}

fn maximal_matches(matches: Vec<CloneMatch>) -> Vec<CloneMatch> {
    let mut candidates = matches
        .iter()
        .filter(|candidate| {
            !matches.iter().any(|other| {
                other != *candidate
                    && other.tokens >= candidate.tokens
                    && contains(&other.left, &candidate.left)
                    && contains(&other.right, &candidate.right)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then(left.changed_tokens.cmp(&right.changed_tokens))
            .then(left.cmp(right))
    });
    let mut selected = Vec::<CloneMatch>::new();
    for candidate in candidates {
        if selected.iter().any(|existing| {
            substantially_overlaps(&existing.left, &candidate.left)
                && substantially_overlaps(&existing.right, &candidate.right)
        }) {
            continue;
        }
        selected.push(candidate);
    }
    let mut by_units = BTreeMap::<(SourceSpan, SourceSpan), CloneMatch>::new();
    for candidate in selected {
        let key = (candidate.left_unit.clone(), candidate.right_unit.clone());
        match by_units.get(&key) {
            Some(existing)
                if (existing.tokens, std::cmp::Reverse(existing.changed_tokens))
                    >= (
                        candidate.tokens,
                        std::cmp::Reverse(candidate.changed_tokens),
                    ) => {}
            _ => {
                by_units.insert(key, candidate);
            }
        }
    }
    by_units.into_values().collect()
}

fn contains(outer: &SourceSpan, inner: &SourceSpan) -> bool {
    outer.path == inner.path
        && outer.start_byte <= inner.start_byte
        && outer.end_byte >= inner.end_byte
}

fn substantially_overlaps(left: &SourceSpan, right: &SourceSpan) -> bool {
    if left.path != right.path {
        return false;
    }
    // overlap is a question about exact offsets, so a span without them
    // cannot answer it
    let (Some((left_start, left_end)), Some((right_start, right_end))) =
        (left.byte_range(), right.byte_range())
    else {
        return false;
    };
    let overlap = left_end
        .min(right_end)
        .saturating_sub(left_start.max(right_start));
    let shorter = left_end
        .saturating_sub(left_start)
        .min(right_end.saturating_sub(right_start));
    shorter > 0 && overlap.saturating_mul(5) >= shorter.saturating_mul(4)
}

fn comparison_unit(file: &TokenizedFile, matched: &SourceSpan) -> SourceSpan {
    // the enclosing unit is found by offset, so a span without offsets is
    // already the best answer available
    let Some((matched_start, matched_end)) = matched.byte_range() else {
        return matched.clone();
    };
    file.units
        .iter()
        .filter(|unit| unit.start_byte <= matched_start && unit.end_byte >= matched_end)
        .min_by_key(|unit| unit.end_byte.saturating_sub(unit.start_byte))
        .map(|unit| SourceSpan {
            path: file.path.clone(),
            start_byte: Some(unit.start_byte),
            end_byte: Some(unit.end_byte),
            start_line: unit.start_line,
            end_line: unit.end_line,
            start_column: None,
            end_column: None,
        })
        .unwrap_or_else(|| matched.clone())
}

pub(crate) struct NearEngine {
    inner: ExactEngine,
    normalizations: Vec<TokenNormalization>,
}

impl NearEngine {
    pub fn new(config: NearConfig) -> Result<Self> {
        if !config.normalize_identifiers && !config.normalize_literals {
            return Err(Error::InvalidConfig(
                "near clones must normalize identifiers, literals, or both".to_owned(),
            ));
        }
        if config.max_changed_percent == 0 || config.max_changed_percent > 100 {
            return Err(Error::InvalidConfig(
                "max-changed-percent must be between 1 and 100".to_owned(),
            ));
        }
        let normalization = Normalization {
            identifiers: config.normalize_identifiers,
            literals: config.normalize_literals,
        };
        let mut normalizations = Vec::new();
        if normalization.identifiers {
            normalizations.push(TokenNormalization::Identifiers);
        }
        if normalization.literals {
            normalizations.push(TokenNormalization::Literals);
        }
        Ok(Self {
            inner: ExactEngine::with_mode(
                config.min_tokens,
                config.min_lines,
                MatchMode::Near {
                    normalization,
                    max_changed_percent: config.max_changed_percent,
                },
            )?,
            normalizations,
        })
    }

    pub fn replace(&mut self, file: TokenizedFile) -> Result<()> {
        self.inner.replace(file)
    }

    pub fn facts(&self) -> Vec<Fact<NearTokenClone>> {
        self.inner
            .matches()
            .into_iter()
            .map(|matched| Fact {
                value: NearTokenClone {
                    left: matched.left,
                    right: matched.right,
                    tokens: matched.tokens,
                    changed_tokens: matched.changed_tokens,
                    normalized_token_sha256: matched.signature_sha256,
                    normalizations: self.normalizations.clone(),
                },
                derivation: Derivation {
                    analyzer: "duplication.near-token".to_owned(),
                    analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
                    inputs: matched.inputs,
                },
            })
            .collect()
    }
}

fn build_windows(
    file_id: u64,
    file: &TokenizedFile,
    size: u32,
    mode: MatchMode,
) -> Vec<WindowRecord> {
    // a u32 exceeds usize only below 32-bit targets
    let Ok(size) = usize::try_from(size) else {
        // straitjacket-allow:error-discard
        return Vec::new();
    };
    if size == 0 || file.tokens.len() < size {
        return Vec::new();
    }
    file.tokens
        .windows(size)
        .map(|tokens| WindowRecord {
            domain: file.comparison_domain.clone(),
            digest: match mode {
                MatchMode::Exact => token_digest(tokens),
                MatchMode::Near { normalization, .. } => {
                    normalized_token_digest(tokens, normalization)
                }
            },
            file: file_id,
            start_token: tokens[0].ordinal,
            end_token: tokens[tokens.len() - 1].ordinal,
            start_byte: tokens[0].start_byte,
            end_byte: tokens[tokens.len() - 1].end_byte,
            start_line: tokens[0].start_line,
            end_line: tokens[tokens.len() - 1].end_line,
        })
        .collect()
}

fn same_tokens(left: &[SyntaxToken], right: &[SyntaxToken]) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| left.kind == right.kind && left.lexeme == right.lexeme)
}

fn span(path: &Path, tokens: &[SyntaxToken]) -> Option<SourceSpan> {
    Some(SourceSpan {
        path: path.to_path_buf(),
        start_byte: Some(tokens.first()?.start_byte),
        end_byte: Some(tokens.last()?.end_byte),
        start_line: tokens.first()?.start_line,
        end_line: tokens.last()?.end_line,
        start_column: None,
        end_column: None,
    })
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start < right_end && right_start < left_end
}
