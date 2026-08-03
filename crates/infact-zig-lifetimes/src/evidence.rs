//! Ownership that only the assignment site can settle.
//!
//! The declaration rules in the crate root answer what syntax alone can answer,
//! and they are safe by construction: none of them can name a class that hands
//! Rust the memory to free. That safety is bought by declining every field
//! whose ownership lives in where the value came from — which is `ARENA`,
//! `SHARED` and `OWNED`, three classes at zero recall and 298 fields of Bun's
//! classification.
//!
//! These rules read the assignments and calls `entl-zig-observe` reports and
//! answer some of those. **They can name an owning class, and that is a
//! different risk.** A wrong `OWNED` is a `Box` over a pointer somebody else
//! frees: a double-free that compiles. So they are a separate type, reached
//! through a separate function, and [`EvidenceRule::can_double_free`] says of
//! each one whether getting it wrong costs a leak or a crash.
//!
//! ## What it costs
//!
//! Scored end to end, adding these to the safe list takes coverage from 41.0%
//! of matched fields to 46.9% and leaves precision at 87.0%. `ARENA` goes from
//! 0% to 39% recall at 100% precision, `SHARED` from 0% to 54% at 86.3%.
//!
//! The price is 10 wrong answers that name an owning class, all from
//! [`EvidenceRule::RefCounted`] — fields on which `deref()` is called but which
//! Bun classified `BORROW_PARAM` or `BACKREF`, because they borrow a counted
//! object whose count somebody else holds. Those 10 are double-frees, not
//! leaks. Tightening the rule does not help: requiring `deref` without `ref`
//! buys 1.7pt of precision for 6.6pt of recall, and requiring both is worse.
//!
//! So [`crate::classify`] keeps its guarantee and this is a separate call. A
//! consumer that cannot accept a double-free should use the declaration rules
//! alone and take the lower coverage.
//!
//! ## Why the allocation rule is not here
//!
//! `allocator.create(T)` assigned to a field looks like the definition of
//! `OWNED`, and in a probe over this corpus it is 72% precise. That is not
//! enough. The 28% are fields allocated by one owner and *handed* to this one —
//! `NetworkTask.streaming_extract_task` takes its value from a preallocated
//! pool, and the pool frees it. A rule at 72% naming `OWNED` would emit a
//! double-free on roughly one field in four it fires on, against a porting
//! guide heuristic that is already 42% precise and merely wrong rather than
//! dangerous.
//!
//! So allocation is observed and not concluded. Separating "this was allocated
//! here" from "this container frees it" needs the free site too, which is a
//! call-graph question. Until that exists, [`EvidenceRule`] emits `OWNED` for
//! nothing.

use entl_zig_observe::{ContainerField, FieldAssignment, MethodCall};

use crate::{Classification, OwnershipClass};

/// What the file that declares a field says about it elsewhere.
///
/// Borrowed rather than owned because a caller has already grouped a file's
/// assignments and calls once and should not copy them per field.
#[derive(Debug, Clone, Copy)]
pub struct FieldEvidence<'a> {
    /// Assignments whose left-hand side names this field.
    pub assignments: &'a [FieldAssignment],
    /// Method calls whose receiver ends in this field's name.
    pub calls: &'a [MethodCall],
}

impl FieldEvidence<'_> {
    /// Is any assigned value written like this?
    fn any_value(&self, predicate: impl Fn(&str) -> bool) -> bool {
        self.assignments
            .iter()
            .any(|assignment| predicate(&assignment.value))
    }

    /// Is any of these methods called on the field?
    fn calls_any(&self, methods: &[&str]) -> bool {
        self.calls
            .iter()
            .any(|call| methods.contains(&call.method.as_str()))
    }
}

/// A rule that needs more than the declaration.
///
/// Ordered as the list tries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceRule {
    /// The value came from a `Store.append`, so an arena owns it and frees it
    /// in bulk. The field never frees anything.
    ArenaStore,
    /// `ref()` or `deref()` is called on the field: it is a counted reference.
    RefCounted,
    /// The value came from a global singleton getter, so the field borrows
    /// something with the lifetime of the process.
    GlobalSingleton,
}

/// Every evidence rule, in decision-list order.
pub const EVIDENCE_RULES: &[EvidenceRule] = &[
    EvidenceRule::ArenaStore,
    EvidenceRule::RefCounted,
    EvidenceRule::GlobalSingleton,
];

/// Getters that hand back a process-lifetime singleton rather than a new value.
const SINGLETON_CALLS: &[&str] = &["initGlobal", "bunVM", "getVM", "vm", "instance"];

impl EvidenceRule {
    pub fn id(self) -> &'static str {
        match self {
            EvidenceRule::ArenaStore => "arena-store",
            EvidenceRule::RefCounted => "ref-counted",
            EvidenceRule::GlobalSingleton => "global-singleton",
        }
    }

    pub fn class(self) -> OwnershipClass {
        match self {
            EvidenceRule::ArenaStore => OwnershipClass::Arena,
            EvidenceRule::RefCounted => OwnershipClass::Shared,
            EvidenceRule::GlobalSingleton => OwnershipClass::Static,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            EvidenceRule::ArenaStore => {
                "the value came from a Store.append, so an arena frees it in bulk"
            }
            EvidenceRule::RefCounted => "ref() or deref() is called on the field",
            EvidenceRule::GlobalSingleton => "the value came from a singleton getter",
        }
    }

    /// Would a wrong answer from this rule hand Rust memory to free that it
    /// does not own?
    ///
    /// `ARENA` and `SHARED` both do: `ARENA` ports to an arena reference and
    /// `SHARED` to an `Rc`, and putting a borrowed pointer in either is a
    /// double-free rather than a leak. `STATIC` does not. A consumer choosing
    /// what to trust should read this before the precision.
    pub fn can_double_free(self) -> bool {
        self.class().is_owning()
    }

    /// Precision against Bun's classification, measured end to end.
    ///
    /// Regenerate with `examples/score.rs`; a number here that was not measured
    /// is worse than none.
    pub fn measured_precision(self) -> f32 {
        match self {
            EvidenceRule::ArenaStore => 100.0,
            EvidenceRule::RefCounted => 86.3,
            EvidenceRule::GlobalSingleton => 75.0,
        }
    }

    /// Worst precision on any one of five folds of files.
    ///
    /// `GlobalSingleton` fires four times in the whole corpus, so its numbers
    /// mean very little and a consumer should treat it as unmeasured rather
    /// than as 75% precise.
    pub fn worst_fold_precision(self) -> f32 {
        match self {
            EvidenceRule::ArenaStore => 100.0,
            EvidenceRule::RefCounted => 60.0,
            EvidenceRule::GlobalSingleton => 0.0,
        }
    }

    /// How many fields the rule fired on when it was measured.
    ///
    /// Carried because a precision over four samples is not a precision, and a
    /// consumer weighing rules needs to see which numbers are load-bearing.
    pub fn measured_sample(self) -> usize {
        match self {
            EvidenceRule::ArenaStore => 23,
            EvidenceRule::RefCounted => 73,
            EvidenceRule::GlobalSingleton => 4,
        }
    }

    fn matches(self, evidence: FieldEvidence<'_>) -> bool {
        match self {
            EvidenceRule::ArenaStore => evidence.any_value(|value| value.contains("Store.append")),
            EvidenceRule::RefCounted => evidence.calls_any(&["ref", "deref"]),
            EvidenceRule::GlobalSingleton => evidence.any_value(|value| {
                SINGLETON_CALLS
                    .iter()
                    .any(|call| value.contains(&format!("{call}(")))
            }),
        }
    }
}

/// The class the assignment evidence implies, or `None` when it does not say.
///
/// Deliberately does not fall back to the declaration rules: a caller that
/// wants both should try [`crate::classify`] first, which is safe, and reach
/// here only for what it declined.
pub fn classify_with_evidence(
    field: &ContainerField,
    evidence: FieldEvidence<'_>,
) -> Option<Classification> {
    if !field.zig_type.contains('*') {
        return None;
    }
    let rule = EVIDENCE_RULES
        .iter()
        .copied()
        .find(|rule| rule.matches(evidence))?;
    Some(Classification {
        class: rule.class(),
        basis: crate::Basis::Evidence(rule),
        measured_precision: rule.measured_precision(),
        worst_fold_precision: rule.worst_fold_precision(),
        span: field.span,
    })
}

/// The classes evidence rules can name that declaration rules cannot.
///
/// Asserted rather than described: the point of this module is that it reaches
/// classes the safe list cannot, and if it stopped doing so it would be dead
/// weight carrying extra risk.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_rules_reach_classes_the_declaration_rules_cannot() {
        let syntactic: Vec<OwnershipClass> = crate::RULES
            .iter()
            .map(|rule| OwnershipClass::from(rule.class()))
            .collect();
        let reached: Vec<OwnershipClass> = EVIDENCE_RULES.iter().map(|rule| rule.class()).collect();
        assert!(
            reached.iter().any(|class| !syntactic.contains(class)),
            "evidence rules add nothing the declaration rules do not already have"
        );
    }

    /// The risk this module carries, made explicit so it cannot drift silently.
    #[test]
    fn owning_evidence_rules_are_flagged_as_such() {
        for rule in EVIDENCE_RULES {
            assert_eq!(
                rule.can_double_free(),
                rule.class().is_owning(),
                "{} misreports its own risk",
                rule.id()
            );
        }
        // ARENA hands Rust an arena reference; getting it wrong is not a leak.
        assert!(EvidenceRule::ArenaStore.can_double_free());
        assert!(!EvidenceRule::GlobalSingleton.can_double_free());
    }

    /// No evidence rule names OWNED. See the module note on why.
    #[test]
    fn nothing_here_concludes_owned() {
        for rule in EVIDENCE_RULES {
            assert_ne!(rule.class(), OwnershipClass::Owned, "{}", rule.id());
        }
    }

    #[test]
    fn a_rule_that_matches_nothing_declines() {
        let evidence = FieldEvidence {
            assignments: &[],
            calls: &[],
        };
        for rule in EVIDENCE_RULES {
            assert!(!rule.matches(evidence), "{} fired on nothing", rule.id());
        }
    }

    /// If no evidence rule could name an owning class, this module would be
    /// carrying extra risk for nothing and should use the safe type instead.
    #[test]
    fn at_least_one_evidence_rule_names_an_owning_class() {
        assert!(EVIDENCE_RULES.iter().any(|rule| rule.class().is_owning()));
    }
}
