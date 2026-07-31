//! Backend-independent facts derived by Infact analyses.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const EXTERNAL_CATALOG_SCHEMA: u32 = 1;
pub const DERIVED_LIBRARY_BEHAVIOR_SCHEMA: u32 = 1;
pub const DERIVED_MACRO_BEHAVIOR_SCHEMA: u32 = 1;
pub const CALL_EFFECT_CATALOG_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub path: PathBuf,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
}

impl SourceSpan {
    pub fn line_count(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InputEvidence {
    pub path: PathBuf,
    pub content_sha256: String,
    pub parser_id: String,
    pub parser_version: String,
    pub grammar_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Derivation {
    pub analyzer: String,
    pub analyzer_version: String,
    pub inputs: Vec<InputEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Fact<T> {
    pub value: T,
    pub derivation: Derivation,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExactTokenClone {
    pub left: SourceSpan,
    pub right: SourceSpan,
    pub tokens: u32,
    pub token_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenNormalization {
    Identifiers,
    Literals,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NearTokenClone {
    pub left: SourceSpan,
    pub right: SourceSpan,
    pub tokens: u32,
    pub changed_tokens: u32,
    pub normalized_token_sha256: String,
    pub normalizations: Vec<TokenNormalization>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallEffectCatalog {
    pub schema: u32,
    pub language: String,
    pub version: String,
    pub calls: Vec<CallEffects>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallEffects {
    pub path: String,
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CallEffectEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallEffectEvidence {
    pub effect: Effect,
    #[serde(alias = "boundary")]
    pub origin: String,
    pub path: Vec<CallEdgeEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallEdgeEvidence {
    pub caller: String,
    pub callee: String,
    pub call: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectTrace {
    pub callable: String,
    pub callable_span: SourceSpan,
    pub effect: Effect,
    pub origin: String,
    pub path: Vec<CallEdgeEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Allocate,
    Block,
    EnvironmentRead,
    EnvironmentWrite,
    FileRead,
    FileWrite,
    Network,
    Process,
    Random,
    Time,
    Unsafe,
}

impl Effect {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allocate => "allocate",
            Self::Block => "block",
            Self::EnvironmentRead => "environment-read",
            Self::EnvironmentWrite => "environment-write",
            Self::FileRead => "file-read",
            Self::FileWrite => "file-write",
            Self::Network => "network",
            Self::Process => "process",
            Self::Random => "random",
            Self::Time => "time",
            Self::Unsafe => "unsafe",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExternalCatalog {
    pub schema: u32,
    pub package: String,
    pub version: String,
    pub rustdoc_format: u32,
    pub source_sha256: String,
    pub callables: Vec<ExternalCallable>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExternalCallable {
    pub path: String,
    pub container: CallableContainer,
    pub signature: CallableSignature,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallableContainer {
    Trait { path: String },
    Module { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallableSignature {
    pub inputs: Vec<CallableParameter>,
    pub output: Option<ExternalType>,
    pub requirements: Vec<TypeRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallableParameter {
    pub name: String,
    pub ty: ExternalType,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TypeRequirement {
    pub subject: ExternalType,
    pub bounds: Vec<ExternalBound>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalBound {
    Trait { path: String },
    Lifetime { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalType {
    Generic {
        name: String,
    },
    Primitive {
        name: String,
    },
    Path {
        path: String,
        arguments: Vec<ExternalType>,
    },
    Reference {
        mutable: bool,
        inner: Box<ExternalType>,
    },
    Associated {
        name: String,
        self_type: Box<ExternalType>,
        trait_path: Option<String>,
    },
    Tuple {
        elements: Vec<ExternalType>,
    },
    Slice {
        element: Box<ExternalType>,
    },
    Array {
        element: Box<ExternalType>,
        length: String,
    },
    Infer,
    Never,
    Unsupported {
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LibraryBehaviorMatch {
    pub target: LibraryTarget,
    pub pattern: LibraryBehaviorPattern,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LibraryTarget {
    Callable {
        package: String,
        version: String,
        path: String,
        catalog_sha256: String,
    },
    DeriveMacro {
        package: String,
        version: String,
        path: String,
        expansion_sha256: String,
    },
}

impl LibraryTarget {
    pub fn path(&self) -> &str {
        match self {
            Self::Callable { path, .. } | Self::DeriveMacro { path, .. } => path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LibraryBehaviorPattern {
    IteratorCollectVecJoin,
    IteratorManualCounts,
    IteratorManualCountsBy,
    IteratorManualGroupMap,
    IteratorManualGroupMapBy,
    IteratorCollectThenSort,
    IteratorCollectThenSortBy,
    IteratorCollectThenSortByKey,
    IteratorCollectThenSortUnstable,
    IteratorCollectThenSortUnstableBy,
    IteratorCollectThenSortUnstableByKey,
    EnumManualDisplay,
    EnumManualAsRefStr,
    EnumManualVariantArray,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DerivedLibraryBehavior {
    pub schema: u32,
    pub callable_package: String,
    pub callable_version: String,
    pub callable_path: String,
    pub catalog_sha256: String,
    pub implementation: Vec<ImplementationEvidence>,
    pub program: NormalizedBehavior,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImplementationEvidence {
    pub callable_path: String,
    pub span: SourceSpan,
    pub source_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NormalizedBehavior {
    pub operations: Vec<NormalizedOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NormalizedOperation {
    CreateMap {
        output: NormalizedValue,
    },
    Iterate {
        input: NormalizedValue,
        item: NormalizedValue,
        body: Vec<NormalizedOperation>,
    },
    Apply {
        function: NormalizedValue,
        input: NormalizedValue,
        output: NormalizedValue,
    },
    DestructurePair {
        input: NormalizedValue,
        first: NormalizedValue,
        second: NormalizedValue,
    },
    IncrementMapEntry {
        map: NormalizedValue,
        key: NormalizedValue,
        amount: u64,
    },
    PushMapEntry {
        map: NormalizedValue,
        key: NormalizedValue,
        value: NormalizedValue,
    },
    CollectVec {
        input: NormalizedValue,
        output: NormalizedValue,
    },
    Sort {
        value: NormalizedValue,
        stability: SortStability,
        comparison: SortComparison,
    },
    IntoIterator {
        input: NormalizedValue,
        output: NormalizedValue,
    },
    Return {
        value: NormalizedValue,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortStability {
    Stable,
    Unstable,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortComparison {
    Natural,
    Comparator { function: NormalizedValue },
    Key { function: NormalizedValue },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NormalizedValue(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DerivedMacroBehavior {
    pub schema: u32,
    pub macro_package: String,
    pub macro_version: String,
    pub derive_path: String,
    pub probe_source_sha256: String,
    pub expansion_sha256: String,
    pub behavior: MacroBehavior,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MacroBehavior {
    EnumDisplay { case: StringCase },
    EnumAsRefStr { case: StringCase },
    EnumVariantArray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StringCase {
    Kebab,
    Snake,
}
