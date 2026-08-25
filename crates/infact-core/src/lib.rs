//! Backend-independent facts derived by Infact analyses.

use std::path::PathBuf;

pub use infact_normalize::{Coverage, Form, Pattern, Resolved};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;

pub const EXTERNAL_CATALOG_SCHEMA: u32 = 1;
pub const DERIVED_LIBRARY_BEHAVIOR_SCHEMA: u32 = 1;
pub const DERIVED_MACRO_BEHAVIOR_SCHEMA: u32 = 1;
pub const CALL_EFFECT_CATALOG_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub path: PathBuf,
    /// Byte offsets into the file as the producer read it.
    ///
    /// A parser knows these because it read the bytes. A compiler reports
    /// lines and columns, so a span it produced has none, and a reporter must
    /// not invent them.
    pub start_byte: Option<u64>,
    pub end_byte: Option<u64>,
    pub start_line: u32,
    pub end_line: u32,
    /// Columns, when the producer supplies them. A whole-line span has none.
    #[serde(default)]
    pub start_column: Option<u32>,
    #[serde(default)]
    pub end_column: Option<u32>,
}

impl SourceSpan {
    /// The byte range, when the producer supplied one.
    ///
    /// Comparisons that need exact offsets — overlap, containment — cannot be
    /// made without them, and should decline rather than assume.
    pub fn byte_range(&self) -> Option<(u64, u64)> {
        Some((self.start_byte?, self.end_byte?))
    }

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
    /// A digest over the parser pack's queries, when a parser produced this.
    ///
    /// A fact derived through a Tree-sitter query depends on that query's text
    /// as much as on the grammar, so two runs with different queries would
    /// otherwise carry indistinguishable provenance. Empty when the input came
    /// from a compiler rather than a parser, which uses no queries at all.
    #[serde(default)]
    pub queries_sha256: String,
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

/// How a fallible expression's error was dropped.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum DiscardForm {
    /// `let _ = fallible();`
    LetUnderscore,
    /// `.ok()`, which turns a cause into an absence.
    OkDiscard,
    /// `.unwrap_or(..)`, `.unwrap_or_default()`, or `.unwrap_or_else(|_| ..)`.
    UnwrapOr,
    /// An `Err(_)` match arm.
    ErrArm,
    /// `if let Ok(..)` or `let Ok(..) = .. else`, where no arm sees the error.
    OkBinding,
    /// `.filter_map(Result::ok)`, which drops failed items mid-iteration.
    IteratorDrop,
    /// `.map_err(|_| ..)`, which keeps the failure but discards its cause.
    CauseErased,
    /// `.unwrap()` or `.expect(..)`, which aborts instead of returning.
    Panic,
}

impl DiscardForm {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Whether the enclosing callable could have reported the failure upward.
///
/// A discard inside an infallible callable is the strongest signal: the error
/// has no route out no matter what the caller does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Containment {
    /// Returns `Result`, so propagation was available and was declined.
    Fallible,
    /// Returns `Option`, so a failure can only leave as an absence.
    Optional,
    /// Returns neither, so the error cannot leave this callable at all.
    Infallible,
}

impl Containment {
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Whether the site is certainly discarding a failure.
///
/// `.ok()` and an `Err(_)` arm name `Result` itself. `let _ =` does not name a
/// type, but the binding exists only to drop a value the compiler flagged as
/// must-use, so the discard is explicit either way. `.unwrap_or_default()`
/// reads the same on `Option`, and syntax cannot tell the two apart, so the
/// analyzer reports what it saw and leaves the policy call to the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Certainty {
    /// The form names `Result`, or exists only to discard a must-use value.
    Certain,
    /// The same form exists on `Option`; the receiver's type was not resolved.
    Possible,
}

/// How far a discarded failure could have travelled up the call graph.
///
/// `Containment` is local: it reads one signature. This is the same question
/// asked of the callers, because a discard inside an infallible callable that
/// is itself only ever called by infallible callables cannot be reported
/// anywhere, no matter what the caller does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reach {
    /// The discarding callable returns `Result`; it could have returned this.
    Local,
    /// A caller above returns `Result`, so the failure could have reached it.
    Ancestor,
    /// Every caller reachable from here is infallible; nothing can report it.
    Sealed,
    /// No caller could be resolved from syntax, so the answer is not known.
    Unknown,
}

/// A site where a fallible expression's error was dropped rather than returned.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ErrorDiscard {
    /// The enclosing callable, or the module when a discard sits outside one.
    pub callable: String,
    pub callable_span: SourceSpan,
    pub form: DiscardForm,
    pub containment: Containment,
    pub certainty: Certainty,
    /// The expression whose error was dropped, as written.
    pub expression: String,
    pub span: SourceSpan,
    /// Whether the site is a test, which a policy usually exempts.
    pub in_test: bool,
    #[serde(default = "unknown_reach")]
    pub reach: Reach,
    /// Callers from the outermost one found down to the discarding callable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<CallEdgeEvidence>,
}

const fn unknown_reach() -> Reach {
    Reach::Unknown
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
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
    pub fn as_str(self) -> &'static str {
        self.into()
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
    /// The declared signature, when the catalog was built from something that
    /// knows types. A catalog derived from source alone has none, and saying so
    /// is better than inventing one.
    #[serde(default)]
    pub signature: Option<CallableSignature>,
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
    /// Other callables the code has equally reimplemented.
    ///
    /// `BTreeMap::keys` and `HashMap::keys` derive to one form because they are
    /// one behavior; which of them a caller reimplemented depends on the type of
    /// the value they wrote it against, and that is not in the syntax. Choosing
    /// between them without knowing the type is inventing certainty — the
    /// previous rule picked the shorter path, so `BTreeMap` code was always told
    /// `HashMap`.
    ///
    /// So a match reports what it established: the behavior, and every API that
    /// behavior belongs to. Type information narrows this to one; until it does,
    /// naming them all is the true answer rather than a lucky one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<LibraryTarget>,
    pub span: SourceSpan,
    /// Whether the code does this and other work in the same pass.
    ///
    /// A fused match is a weaker claim: the behavior is present, but replacing
    /// it with the library call is not a mechanical substitution, because
    /// something else is interleaved with it.
    #[serde(default)]
    pub fused: bool,
    /// What has to hold for the swap to be sound, that this cannot check.
    ///
    /// A match says the code computes what the API computes. It does not say
    /// the two are interchangeable here, and for some behaviors they are not:
    /// the API may need a stronger bound on the element type, or allocate where
    /// the code does not, or reach the same answer by a different route that a
    /// caller could tell apart. None of that is in the syntax.
    ///
    /// Reporting the gap is what makes this a recommendation rather than a
    /// lint. Where the gap IS visible — a `const fn` that cannot allocate at
    /// all — the recognizer refuses instead, because a condition a reader must
    /// check is worse than a finding they never see.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

/// Something a recommendation depends on that the syntax does not settle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Condition {
    /// The API needs a bound on the element type that the code does not.
    ///
    /// A pairwise `==` needs only `PartialEq`; reaching the same answer through
    /// a hash set needs `Eq + Hash`. The difference is not pedantic: `f64` is
    /// `PartialEq` and not `Eq`, and two `NaN`s are unequal to each other, so
    /// the loop calls them distinct and the set cannot be built at all.
    ElementBound {
        requires: String,
        code_requires: String,
    },
    /// The API allocates where the code does not.
    Allocates,
    /// The API reaches the answer without making the comparisons the code makes.
    ///
    /// An operator with an observable effect — one that logs, counts, or panics
    /// — runs a quadratic number of times here and need not run at all there.
    ComparisonObservable,
    /// The code is cheaper at the sizes it is actually called with.
    ///
    /// A quadratic scan of four elements beats allocating a hash set. Which one
    /// this is depends on the caller, and nothing in the callee says.
    SmallInputsFavourTheCode,
}

impl std::fmt::Display for Condition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ElementBound {
                requires,
                code_requires,
            } => write!(
                formatter,
                "the element type must be {requires}; the code needs only {code_requires}"
            ),
            Self::Allocates => formatter.write_str("the API allocates and the code does not"),
            Self::ComparisonObservable => formatter.write_str(
                "a comparison with an observable effect runs here and need not run there",
            ),
            Self::SmallInputsFavourTheCode => {
                formatter.write_str("the code is faster at small sizes")
            }
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DerivedLibraryBehavior {
    pub schema: u32,
    pub callable_package: String,
    pub callable_version: String,
    pub callable_path: String,
    pub catalog_sha256: String,
    pub implementation: Vec<ImplementationEvidence>,
    pub program: Form,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImplementationEvidence {
    pub callable_path: String,
    pub span: SourceSpan,
    pub source_sha256: String,
}

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
