//! Which Rust ownership a Zig pointer field implies, where syntax settles it.
//!
//! Zig spells every pointer `*Foo` whether the container owns the memory,
//! borrows it from a caller, points back at a parent, or threads an intrusive
//! list. Rust needs the distinction in the type, and getting it wrong produces
//! a double-free or a leak that compiles perfectly.
//!
//! The porting guide answers this by reading `deinit` and checking whether the
//! field is freed there. Measured against Bun's own hand-built classification
//! of 2,252 fields, that heuristic fires on 102 of them and is **42% precise** —
//! it collects 43 `OWNED`, but also 21 `FFI` freed by an extern destroy call and
//! 20 `BORROW_PARAM` freed by whoever lent the pointer.
//!
//! This does something narrower and much more precise. It is a decision list of
//! six syntactic rules over container fields observed by `entl-zig-observe`.
//! Scored end to end against those 2,252 fields — parsing Bun with tree-sitter,
//! observing, then classifying — it answers **41.0%** of the fields it can match
//! at **86.7%** precision, and abstains on the rest.
//!
//! The observer reaches 1,691 of the 2,252 classified fields (75.1%); the
//! remainder are container paths the two spell differently, not fields it
//! cannot see. Percentages here are over what was matched, and
//! `examples/score.rs` prints both counts so the denominator is never hidden.
//!
//! ## Abstention is the design, not a limitation
//!
//! The classes split by whether they leave a syntactic trace. `FFI` does — the
//! container is `extern`, so a C declaration decides the layout. `JSC_BORROW`
//! does — the type names a JavaScriptCore type. `STATIC` does — the field is a
//! function pointer. `OWNED` does not: an owned `*Foo` and a borrowed `*Foo` are
//! written identically, and only where the memory is allocated and freed tells
//! them apart. Measured, a classifier over syntax alone reaches F1 42 on
//! `OWNED` against 84 on `JSC_BORROW`.
//!
//! So this deliberately answers only the questions syntax can answer. What it
//! declines is not a gap to be papered over with a guess; it is the input to a
//! reachability pass or to a model, and both are better spent on 61% of the
//! fields than on 100%.
//!
//! ## The property that makes a wrong answer survivable
//!
//! **No rule here can emit `OWNED` or `SHARED`**, and that is enforced by
//! [`Rule::class`] being total over an enum that excludes them. Every class it
//! can emit — `FFI`, `INTRUSIVE`, `BACKREF`, `JSC_BORROW`, `STATIC` — ports to a
//! raw pointer or a borrow, never to a `Box` or an `Rc`. A wrong answer from
//! this crate therefore cannot cause a double-free. Of the 693 fields it
//! classifies, 9 (1.3%) name a field whose true class owns its memory, so the
//! realistic worst case is a leak. The other 83 errors swap one non-owning
//! class for another, which port to a raw pointer or a borrow either way.
//!
//! That asymmetry is the whole reason to prefer a narrow rule list over a
//! higher-accuracy classifier that answers everything. A model that reaches 61%
//! overall will say `OWNED` sometimes, and when it is wrong there, the port
//! double-frees.
//!
//! ## Precision is reported, not enforced
//!
//! Every [`Classification`] carries the rule that produced it and that rule's
//! [`Rule::measured_precision`], along with the worst per-fold precision behind
//! it. Deciding which precision is good enough is policy and belongs to a
//! consumer; this crate reports what it measured and suppresses nothing.

use entl_zig_observe::{ContainerField, Span};

/// The ownership classes Bun's port uses, as spelled in its classification.
///
/// The full set is listed because a consumer reads it from that file and needs
/// to name all of them. Only the subset in [`EmittableClass`] can be derived
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwnershipClass {
    Owned,
    Shared,
    BorrowParam,
    BorrowField,
    JscBorrow,
    Backref,
    Intrusive,
    Arena,
    Static,
    Ffi,
    Unknown,
}

impl OwnershipClass {
    pub fn label(self) -> &'static str {
        match self {
            OwnershipClass::Owned => "OWNED",
            OwnershipClass::Shared => "SHARED",
            OwnershipClass::BorrowParam => "BORROW_PARAM",
            OwnershipClass::BorrowField => "BORROW_FIELD",
            OwnershipClass::JscBorrow => "JSC_BORROW",
            OwnershipClass::Backref => "BACKREF",
            OwnershipClass::Intrusive => "INTRUSIVE",
            OwnershipClass::Arena => "ARENA",
            OwnershipClass::Static => "STATIC",
            OwnershipClass::Ffi => "FFI",
            OwnershipClass::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "OWNED" => OwnershipClass::Owned,
            "SHARED" => OwnershipClass::Shared,
            "BORROW_PARAM" => OwnershipClass::BorrowParam,
            "BORROW_FIELD" => OwnershipClass::BorrowField,
            "JSC_BORROW" => OwnershipClass::JscBorrow,
            "BACKREF" => OwnershipClass::Backref,
            "INTRUSIVE" => OwnershipClass::Intrusive,
            "ARENA" => OwnershipClass::Arena,
            "STATIC" => OwnershipClass::Static,
            "FFI" => OwnershipClass::Ffi,
            "UNKNOWN" => OwnershipClass::Unknown,
            _ => return None,
        })
    }

    /// Does porting this class as written hand the field's memory to Rust to
    /// free? These are the classes where a wrong answer double-frees, and no
    /// rule in this crate can produce one.
    pub fn is_owning(self) -> bool {
        matches!(
            self,
            OwnershipClass::Owned | OwnershipClass::Shared | OwnershipClass::Arena
        )
    }
}

/// The classes a syntactic rule is allowed to conclude.
///
/// A separate type rather than a runtime check, so that "this crate cannot say
/// `OWNED`" is a fact about the code rather than a promise in a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EmittableClass {
    Ffi,
    Intrusive,
    Backref,
    JscBorrow,
    Static,
}

impl From<EmittableClass> for OwnershipClass {
    fn from(class: EmittableClass) -> Self {
        match class {
            EmittableClass::Ffi => OwnershipClass::Ffi,
            EmittableClass::Intrusive => OwnershipClass::Intrusive,
            EmittableClass::Backref => OwnershipClass::Backref,
            EmittableClass::JscBorrow => OwnershipClass::JscBorrow,
            EmittableClass::Static => OwnershipClass::Static,
        }
    }
}

/// Field names Zig and C both use for a link in an intrusive list.
///
/// A closed list rather than a prefix match: `next_tick_queue` is a queue the
/// container owns, not a link into someone else's list.
const LINK_NAMES: &[&str] = &[
    "next",
    "prev",
    "next_req",
    "prev_req",
    "head",
    "tail",
    "next_free",
    "next_to_free",
    "endgame_next",
];

/// JavaScriptCore types a Zig field can hold but never owns: the JSC heap and
/// its garbage collector own them.
const JSC_TYPES: &[&str] = &[
    "JSGlobalObject",
    "JSPromise",
    "JSValue",
    "JSObject",
    "JSFunction",
    "JSCell",
    "JSString",
    "VirtualMachine",
    "CallFrame",
];

/// One rule, in the order the decision list tries them.
///
/// Order matters and is not arbitrary. [`Rule::ExternLink`] must precede
/// [`Rule::Extern`]: libuv's request structs are `extern` *and* thread intrusive
/// lists, and letting `Extern` claim them first drops those 48 fields from 75%
/// precision to 25%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    /// An `extern` container whose field is named like a list link.
    ExternLink,
    /// Any other field of an `extern` container: a C declaration owns the
    /// layout, so Zig is not managing this memory.
    Extern,
    /// The type names a JavaScriptCore type, which the JSC heap owns.
    JscType,
    /// A function pointer. Code has static storage duration.
    FnPointer,
    /// A field named like a list link, in a container Zig itself declares.
    LinkName,
    /// A field named `parent` or `owner`: a back-pointer up a structure that
    /// owns this one, never freed by the container holding it.
    ParentName,
}

/// Every rule, in decision-list order.
pub const RULES: &[Rule] = &[
    Rule::ExternLink,
    Rule::Extern,
    Rule::JscType,
    Rule::FnPointer,
    Rule::LinkName,
    Rule::ParentName,
];

impl Rule {
    pub fn id(self) -> &'static str {
        match self {
            Rule::ExternLink => "extern-link",
            Rule::Extern => "extern-container",
            Rule::JscType => "jsc-type",
            Rule::FnPointer => "fn-pointer",
            Rule::LinkName => "link-name",
            Rule::ParentName => "parent-name",
        }
    }

    pub fn class(self) -> EmittableClass {
        match self {
            Rule::ExternLink | Rule::LinkName => EmittableClass::Intrusive,
            Rule::Extern => EmittableClass::Ffi,
            Rule::JscType => EmittableClass::JscBorrow,
            Rule::FnPointer => EmittableClass::Static,
            Rule::ParentName => EmittableClass::Backref,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Rule::ExternLink => "the container is `extern` and the field is named like a list link",
            Rule::Extern => "the container is `extern`, so a C declaration owns the layout",
            Rule::JscType => "the type names a JavaScriptCore type, owned by the JSC heap",
            Rule::FnPointer => "the field is a function pointer, and code is static",
            Rule::LinkName => "the field is named like an intrusive list link",
            Rule::ParentName => "the field is named like a back-pointer to an owning parent",
        }
    }

    /// Precision against Bun's hand-classified fields, in percent.
    ///
    /// Measured end to end — parse, observe, classify — with each rule seeing
    /// only what the rules before it declined, which is how the list runs.
    /// Regenerate with `examples/score.rs` if the rule set changes; a number
    /// here that was not measured is worse than none.
    pub fn measured_precision(self) -> f32 {
        match self {
            Rule::ExternLink => 75.0,
            Rule::Extern => 91.6,
            Rule::JscType => 83.2,
            Rule::FnPointer => 76.4,
            Rule::LinkName => 90.3,
            Rule::ParentName => 81.8,
        }
    }

    /// The worst precision this rule reached on any one of five folds of files.
    ///
    /// Reported next to the mean because three of these rules are carried by a
    /// single subsystem and collapse outside it. `ExternLink` is ~100% on
    /// libuv's request structs and 8% on the fold holding c-ares, whose
    /// `struct_ares_*` links Bun classified `FFI` rather than `INTRUSIVE` —
    /// arguably an inconsistency in the classification, since both are intrusive
    /// links inside C structs, but it is the answer key and it is scored as one.
    ///
    /// A consumer that needs a guarantee that holds on a subsystem it has not
    /// seen should read this number, not the mean.
    pub fn worst_fold_precision(self) -> f32 {
        match self {
            Rule::ExternLink => 8.0,
            Rule::Extern => 76.0,
            Rule::JscType => 77.0,
            Rule::FnPointer => 42.0,
            Rule::LinkName => 0.0,
            Rule::ParentName => 50.0,
        }
    }

    fn matches(self, field: &ContainerField) -> bool {
        match self {
            Rule::ExternLink => field.container_kind.is_extern() && is_link_name(&field.name),
            Rule::Extern => field.container_kind.is_extern(),
            Rule::JscType => names_jsc_type(&field.zig_type),
            Rule::FnPointer => is_fn_pointer(&field.zig_type),
            Rule::LinkName => is_link_name(&field.name),
            Rule::ParentName => {
                let name = normalise(&field.name);
                name == "parent" || name == "owner"
            }
        }
    }
}

/// Zig private fields are written `#name`; the classification drops the sigil.
fn normalise(name: &str) -> String {
    name.trim_start_matches(['#', '_']).to_ascii_lowercase()
}

fn is_link_name(name: &str) -> bool {
    let name = normalise(name);
    LINK_NAMES.contains(&name.as_str())
}

fn names_jsc_type(zig_type: &str) -> bool {
    identifiers(zig_type).any(|word| JSC_TYPES.contains(&word))
}

/// Identifier-shaped words in a type expression, so `JSGlobalObjectExtra` does
/// not match `JSGlobalObject` and `jsc.JSValue` does.
fn identifiers(zig_type: &str) -> impl Iterator<Item = &str> {
    zig_type
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
}

/// A function pointer, which is `*const fn (...)` or `?*const fn (...)` in Zig.
///
/// Checked as `fn` followed by a parameter list rather than as a substring, so
/// a type merely named `Transform` does not match.
fn is_fn_pointer(zig_type: &str) -> bool {
    let mut rest = zig_type;
    while let Some(at) = rest.find("fn") {
        let before_is_boundary = rest[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        let after = rest[at + 2..].trim_start();
        if before_is_boundary && after.starts_with('(') {
            return true;
        }
        rest = &rest[at + 2..];
    }
    false
}

/// One derived answer, with what produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub class: OwnershipClass,
    pub rule: Rule,
    /// See [`Rule::measured_precision`]. Carried on the fact so a consumer never
    /// has to look the number up to weigh the answer.
    pub measured_precision: f32,
    pub worst_fold_precision: f32,
    /// The declaration this was derived from.
    pub span: Span,
}

/// The class syntax implies for this field, or `None` when syntax does not say.
///
/// `None` is a real answer and the common one: it means the field needs
/// allocation-to-free evidence, not a guess.
pub fn classify(field: &ContainerField) -> Option<Classification> {
    if !mentions_pointer(&field.zig_type) {
        return None;
    }
    let rule = RULES.iter().copied().find(|rule| rule.matches(field))?;
    Some(Classification {
        class: rule.class().into(),
        rule,
        measured_precision: rule.measured_precision(),
        worst_fold_precision: rule.worst_fold_precision(),
        span: field.span,
    })
}

/// Is there a pointer in this type at all?
///
/// Ownership is a question about a pointer. A `u32` field has an answer to it
/// only in the trivial sense, and reporting one would put noise in front of a
/// consumer counting how much of the real question is covered.
fn mentions_pointer(zig_type: &str) -> bool {
    zig_type.contains('*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_pointer_needs_a_parameter_list() {
        assert!(is_fn_pointer("*const fn (*anyopaque) void"));
        assert!(is_fn_pointer("?*const fn(*anyopaque) callconv(.C) void"));
        assert!(!is_fn_pointer("*Transform"));
        assert!(!is_fn_pointer("*fnord"));
    }

    #[test]
    fn jsc_types_match_on_whole_identifiers() {
        assert!(names_jsc_type("*jsc.JSGlobalObject"));
        assert!(names_jsc_type("?*JSValue"));
        assert!(!names_jsc_type("*JSGlobalObjectProxy"));
        assert!(!names_jsc_type("*Value"));
    }

    #[test]
    fn private_field_sigil_is_ignored() {
        assert!(is_link_name("#next"));
        assert!(is_link_name("next"));
        assert!(!is_link_name("next_tick_queue"));
    }

    /// The safety property this crate rests on, asserted rather than promised.
    #[test]
    fn no_rule_can_conclude_an_owning_class() {
        for rule in RULES {
            let class: OwnershipClass = rule.class().into();
            assert!(
                !class.is_owning(),
                "{} would let a wrong answer double-free",
                rule.id()
            );
        }
    }

    #[test]
    fn every_rule_is_in_the_decision_list() {
        // Adding a variant without listing it would silently disable it.
        let all = [
            Rule::ExternLink,
            Rule::Extern,
            Rule::JscType,
            Rule::FnPointer,
            Rule::LinkName,
            Rule::ParentName,
        ];
        for rule in all {
            assert!(RULES.contains(&rule), "{} is not in RULES", rule.id());
        }
        assert_eq!(RULES.len(), all.len());
    }
}
