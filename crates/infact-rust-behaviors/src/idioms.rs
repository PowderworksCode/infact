//! Recognizing an algorithm written out, and what it would take to replace it.
//!
//! Derivation matches repository code against a form derived from a library's
//! own implementation, which works whenever the library wrote the thing the way
//! a caller would. Sometimes it did not. No library checks that a collection's
//! elements are distinct by comparing every pair; `itertools::all_unique`
//! reaches the same answer through a hash set, so its derived form is a hash
//! set and the quadratic loop it exists to replace matches nothing.
//!
//! What is left is a shape that has to be named directly. That is what the
//! strum recognizers already do — `enum_shapes` states a shape a derive macro
//! would have produced and counts what a query cannot. This states its shapes
//! over the normalized form instead of the syntax tree, which is what lets one
//! recognizer cover spellings that share no syntax.
//!
//! The recognizers here are deliberately narrow. A false positive costs more
//! than a miss: a recommendation that is wrong one time in ten is a
//! recommendation that gets switched off, and every shape below refuses
//! anything it cannot account for.

use infact_core::{
    Condition, Coverage, ExternalBound, ExternalCallable, ExternalType, Form, Pattern, Resolved,
};

/// Why a candidate yielded no recommendation.
///
/// Separate from [`infact_behaviors::Refusal`], which says why a *library*
/// callable yielded no behavior. These say why code that looked like an idiom
/// is not one, or is one that must not be recommended against, and they are
/// worth counting apart: the first bounds what the recognizer can see and the
/// second bounds what it is willing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdiomRefusal {
    /// The form does not have the shape at all.
    NotThisShape,
    /// The walk decides something, but not by comparing the pair it is given.
    ///
    /// A pairwise walk that tests a relation other than equality is asking a
    /// different question: `a < b` over every pair is a sortedness check, and
    /// recommending a distinctness API for it would be wrong.
    DecidesSomethingElse,
    /// The walk computes rather than decides.
    ///
    /// Escaping with a value built from the elements means the pairs are being
    /// used for their content, not merely for whether a duplicate exists.
    EscapesWithAValue,
    /// The code cannot call an allocating API.
    ///
    /// A `const fn` has no allocator. This is the one condition below that is
    /// visible in the syntax, so it is refused rather than reported: a reader
    /// should not be handed a recommendation they must then reject.
    CannotAllocate,
}

/// An algorithm recognized in written-out form.
///
/// Each one is a walk over pairs that decides something, and they differ in
/// three ways only: which pairs the walk has to reach, what it asks of a pair,
/// and what has to hold before the API may be recommended in its place. Adding
/// one is answering those three questions, not writing another recognizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idiom {
    /// Deciding that no two elements of one sequence are equal.
    AllDifferent,
    /// Deciding that each element is no greater than the one after it.
    IsSorted,
}

impl Idiom {
    /// Every algorithm this recognizes.
    pub const ALL: &'static [Self] = &[Self::AllDifferent, Self::IsSorted];

    /// The API this recommends, as a path into the package that offers it.
    pub const fn callable_path(&self) -> (&'static str, &'static str) {
        match self {
            Self::AllDifferent => ("itertools", "itertools::Itertools::all_unique"),
            Self::IsSorted => ("core", "slice::is_sorted"),
        }
    }

    /// Which pairs a walk must reach to decide this.
    ///
    /// Distinctness is a question about every pair, and it does not matter how
    /// often each is seen or in which order. Sortedness is a question about
    /// NEIGHBOURS: the same test over every pair decides something much
    /// stronger, so admitting the other coverages here would report a sortedness
    /// API for code that checks far more than sortedness.
    const fn coverages(&self) -> &'static [Coverage] {
        match self {
            Self::AllDifferent => &[Coverage::Once, Coverage::BothWays],
            Self::IsSorted => &[Coverage::Adjacent],
        }
    }

    /// Whether the recommended API needs an allocator.
    const fn allocates(&self) -> bool {
        match self {
            Self::AllDifferent => true,
            Self::IsSorted => false,
        }
    }

    /// What the code needs of its elements to compute this at all.
    ///
    /// The other half of a [`Condition::ElementBound`] comes from the catalog.
    /// This half is a property of the written-out shape: comparing two elements
    /// with `==` is all a distinctness check asks of them, and `>` is all a
    /// sortedness check asks.
    const fn element_bound_of_the_code(&self) -> &'static str {
        match self {
            Self::AllDifferent => "PartialEq",
            Self::IsSorted => "PartialOrd",
        }
    }

    /// Whether a test asks this idiom's question of a pair.
    ///
    /// Distinctness asks whether the two are equal, and equality reads the same
    /// either way round. Sortedness asks whether the EARLIER one is greater,
    /// and reading that backwards is the opposite question, so the order the
    /// walk bound them in is part of the test.
    fn tests_the_pair(&self, condition: &Form, left: u32, right: u32) -> bool {
        let Form::Binary {
            operator,
            left: first,
            right: second,
        } = condition
        else {
            return false;
        };
        let named = |form: &Form, index: u32| *form == Form::Local(index);
        match self {
            Self::AllDifferent => {
                operator == "=="
                    && ((named(first, left) && named(second, right))
                        || (named(first, right) && named(second, left)))
            }
            // `a > b` and `b < a` are one test written two ways.
            Self::IsSorted => {
                (operator == ">" && named(first, left) && named(second, right))
                    || (operator == "<" && named(first, right) && named(second, left))
            }
        }
    }

    /// What has to hold for the recommendation to be sound.
    ///
    /// Fixed per idiom rather than per finding, because these are properties of
    /// the two implementations rather than of the code that was matched. They
    /// are stated in full even when several will usually be satisfied: a reader
    /// who can dismiss three of them in a second is better served than one who
    /// is not told about the fourth.
    ///
    /// The element bound is read off the catalog rather than written here.
    /// Naming a bound from memory would be a claim about a version of a library
    /// that nothing checked, and it is exactly the claim a reader is least able
    /// to verify. When the two sides agree there is nothing for a reader to
    /// check, and saying so anyway is noise that makes the real conditions
    /// harder to see.
    pub fn conditions(&self, callable: &ExternalCallable) -> Vec<Condition> {
        let mut conditions = Vec::new();
        let code_requires = self.element_bound_of_the_code();
        if let Some(requires) = element_bound(callable)
            && requires != code_requires
        {
            conditions.push(Condition::ElementBound {
                requires,
                code_requires: code_requires.to_owned(),
            });
        }
        match self {
            Self::AllDifferent => conditions.extend([
                Condition::Allocates,
                Condition::ComparisonObservable,
                Condition::SmallInputsFavourTheCode,
            ]),
            // Reaching the answer costs the same either way — one pass, the
            // same comparisons, no allocation — so the only gap is what the two
            // do where the order runs out.
            Self::IsSorted => conditions.push(Condition::IncomparableElements),
        }
        conditions
    }
}

/// Whether a catalogued callable answers a yes-or-no question about a sequence.
///
/// The recognizer names one API by path, and a path is not a promise: a catalog
/// is generated data, and the callable behind a path can change between
/// versions. Checking that it still takes the receiver and still returns `bool`
/// is what keeps a recommendation from being made against a signature nobody
/// read.
pub fn answers_a_predicate(callable: &ExternalCallable) -> bool {
    let Some(signature) = &callable.signature else {
        return false;
    };
    let returns_bool = matches!(
        &signature.output,
        Some(ExternalType::Primitive { name }) if name == "bool"
    );
    returns_bool && signature.inputs.iter().any(|input| input.name == "self")
}

/// The bounds a callable puts on the elements it walks.
///
/// A requirement whose subject is the iterator's own `Item` is a requirement on
/// the elements; everything else constrains the iterator or its lifetimes and
/// is not what a caller has to check about their data.
fn element_bound(callable: &ExternalCallable) -> Option<String> {
    let signature = callable.signature.as_ref()?;
    let bounds = signature
        .requirements
        .iter()
        .filter(|requirement| {
            matches!(&requirement.subject, ExternalType::Associated { name, .. } if name == "Item")
        })
        .flat_map(|requirement| &requirement.bounds)
        .filter_map(|bound| match bound {
            ExternalBound::Trait { path } => Some(path.rsplit("::").next().unwrap_or(path)),
            ExternalBound::Lifetime { .. } => None,
        })
        .collect::<Vec<_>>();
    (!bounds.is_empty()).then(|| bounds.join(" + "))
}

/// Whether a function may allocate.
///
/// The recognizer is handed this rather than reading it, because whether a
/// language has such a context at all is a frontend question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    pub can_allocate: bool,
}

/// The shape of a pairwise decision, as a pattern to be matched.
///
/// Written as a `Form` and unified by the same machinery a derived library
/// behavior goes through, rather than as a walk over the subject by hand. What
/// that buys is not brevity but the parts: unification records what each hole
/// stood for, so the conditions below are stated about the pieces it found
/// instead of re-found by a second traversal that can disagree with the first.
///
/// The holes are deliberately wide. Hole 1 is the test and hole 2 the reaction,
/// and admitting anything there is what leaves every judgement about them to
/// [`all_different`], which can then say WHICH of them was wrong. A pattern
/// narrow enough to reject on its own would only ever answer "no".
fn pairwise_decision(coverage: Coverage) -> Form {
    Form::Pairwise {
        sequence: Box::new(Form::Free(0)),
        left: Box::new(Pattern::Binding(0)),
        right: Box::new(Pattern::Binding(1)),
        body: Box::new(Form::Branch {
            condition: Box::new(Form::Free(1)),
            consequence: Box::new(Form::Free(2)),
            // A test with an `else` is choosing between two things to do, and
            // only one of them is being described here.
            alternative: None,
        }),
        coverage,
    }
}

/// A recognized algorithm, and what a caller needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    /// The sequence the check is over, which is what the recommended call goes
    /// on.
    pub sequence: Form,
    /// The shape as it was matched, for locating it among the statements.
    ///
    /// The recognizer's pattern leaves the sequence a hole and admits either
    /// coverage, so on its own it would locate the first walk in the function
    /// rather than the one recognized. Both are filled back in here.
    pub shape: Form,
}

/// Recognize one algorithm in a normalized function body.
///
/// The shape is a walk over pairs of one sequence that, on a pair answering the
/// idiom's test, does something that does not depend on WHICH pair it found.
/// That is the whole claim: a walk like that computes exactly whether such a
/// pair exists, which is what the recommended API answers. What the code then
/// does with the answer — return it, print it, set a flag — is the caller's
/// business and does not change what the loop computed.
///
/// For distinctness either of the unordered coverages will do, and that is
/// worth saying explicitly: a square guarded loop reaches each pair twice, and
/// every reaction accepted below is idempotent — returning twice is returning,
/// setting a flag to the same constant twice is setting it. The reaction that
/// is NOT idempotent is counting, and counting is refused for its own reasons.
pub fn recognize(idiom: Idiom, form: &Form, context: Context) -> Result<Recognized, IdiomRefusal> {
    // Why the nearest thing to the shape was not it. Finding no walk at all is
    // the uninformative answer, so anything else outranks it.
    let mut refusal = IdiomRefusal::NotThisShape;
    for coverage in idiom.coverages() {
        let pattern = pairwise_decision(*coverage);
        for resolved in form.resolve_all(&pattern) {
            match decides(idiom, &resolved) {
                Ok(sequence) => {
                    if idiom.allocates() && !context.can_allocate {
                        return Err(IdiomRefusal::CannotAllocate);
                    }
                    let mut shape = pattern.clone();
                    if let Form::Pairwise { sequence, .. } = &mut shape {
                        **sequence = resolved.hole(0).cloned().unwrap_or(Form::Literal);
                    }
                    return Ok(Recognized { sequence, shape });
                }
                Err(IdiomRefusal::NotThisShape) => {}
                Err(specific) => refusal = specific,
            }
        }
    }
    Err(refusal)
}

/// Recognize a distinctness check, which is the idiom this began with.
pub fn all_different(form: &Form, context: Context) -> Result<Recognized, IdiomRefusal> {
    recognize(Idiom::AllDifferent, form, context)
}

/// Whether one match of the pairwise shape decides the idiom's question.
///
/// Every question here is asked of a piece the matcher handed over, and the two
/// names are the subject's own — a pattern numbers its roles by its own
/// counting, and asking whether the reaction mentions the pattern's `Local(0)`
/// would be asking about the wrong function's names.
fn decides(idiom: Idiom, resolved: &Resolved) -> Result<Form, IdiomRefusal> {
    let (Some(sequence), Some(condition), Some(consequence)) =
        (resolved.hole(0), resolved.hole(1), resolved.hole(2))
    else {
        return Err(IdiomRefusal::NotThisShape);
    };
    let (Some(left), Some(right)) = (resolved.local(0), resolved.local(1)) else {
        return Err(IdiomRefusal::NotThisShape);
    };
    if !idiom.tests_the_pair(condition, left, right) {
        return Err(IdiomRefusal::DecidesSomethingElse);
    }
    // Reading either element means the pair is being used for its content, and
    // an API that answers only whether such a pair exists cannot supply that.
    if consequence.references_local(left) || consequence.references_local(right) {
        return Err(IdiomRefusal::EscapesWithAValue);
    }
    if !records_that_one_exists(consequence) {
        return Err(IdiomRefusal::NotThisShape);
    }
    Ok(sequence.clone())
}

/// Whether a reaction to a matching pair records only that one was found.
///
/// Two spellings, and between them they are how the check is written. Leaving
/// the function ends the walk, so nothing after it runs and the duplicate has
/// decided the outcome. Assigning a constant to a name from outside sets the
/// flag that is read afterwards.
///
/// `continue` and `break` are neither, and refusing them is most of what keeps
/// this honest: measured across CodeNet, `continue` is the commonest thing a
/// pairwise equality test does by a factor of six, and it is skipping duplicate
/// pairs inside a larger computation rather than testing for them. `count += 1`
/// is refused for the same reason — it counts duplicates, and knowing one
/// exists does not give you how many.
fn records_that_one_exists(consequence: &Form) -> bool {
    match consequence {
        Form::Return(_) => true,
        // A constant is a flag. A value built from what is already there is an
        // accumulation, which is a different question about the same pairs.
        Form::Assign {
            operator,
            target,
            value,
        } => {
            operator == "="
                && matches!(target.as_ref(), Form::Local(_) | Form::Free(_))
                && matches!(value.as_ref(), Form::Constant(_))
        }
        // One reaction is routinely written as several steps: `println!("no");
        // return;` and `ans = "No"; break;` are both in the corpus. It is still
        // only recording that a duplicate exists as long as one step does that
        // and no step does anything else — every other step must be reporting
        // or leaving, because those change nothing about the answer.
        Form::Sequence(steps) => {
            steps.iter().any(records_that_one_exists) && steps.iter().all(is_inert_beside_recording)
        }
        _ => false,
    }
}

/// Whether a step changes nothing about whether a duplicate was found.
///
/// Leaving a loop early is an optimization on a walk whose answer is already
/// settled, and reporting is how the answer gets out. Anything else — another
/// assignment, a call, a push — is work this recognizer has not accounted for,
/// and a reaction it cannot account for is one it must not summarize.
///
/// Any macro counts as reporting, which is looser than it sounds and looser
/// than it reads. The corpus spells the report `println!`, `write!`, `p!` and
/// `echo!` — the last two being the submitter's own — so a list of known names
/// would be a list of one corpus's habits. What bounds the damage is that the
/// caller has already established the reaction cannot mention either element,
/// so a macro here cannot carry the pair out; the residual risk is a macro with
/// an unrelated effect, which makes the finding fused rather than wrong.
fn is_inert_beside_recording(step: &Form) -> bool {
    records_that_one_exists(step)
        || matches!(step, Form::Opaque { kind, .. }
            if kind == "break_expression"
                || kind == "continue_expression"
                || kind.starts_with("macro:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Context {
        Context { can_allocate: true }
    }

    fn pairwise(body: Form) -> Form {
        Form::Sequence(vec![
            Form::Pairwise {
                sequence: Box::new(Form::Free(0)),
                left: Box::new(Pattern::Binding(0)),
                right: Box::new(Pattern::Binding(1)),
                body: Box::new(body),
                coverage: infact_core::Coverage::Once,
            },
            Form::Constant("true".to_owned()),
        ])
    }

    fn escaping(condition: Form, escaped: Form) -> Form {
        Form::Branch {
            condition: Box::new(condition),
            consequence: Box::new(Form::Return(Box::new(escaped))),
            alternative: None,
        }
    }

    fn equal_pair() -> Form {
        Form::Binary {
            operator: "==".to_owned(),
            left: Box::new(Form::Local(0)),
            right: Box::new(Form::Local(1)),
        }
    }

    #[test]
    fn a_pairwise_equality_check_is_all_different() {
        let form = pairwise(escaping(equal_pair(), Form::Constant("false".to_owned())));
        assert_eq!(
            all_different(&form, allowed()).map(|found| found.sequence),
            Ok(Form::Free(0))
        );
    }

    /// A relation other than equality asks a different question.
    #[test]
    fn a_pairwise_ordering_check_is_not_all_different() {
        let ordered = Form::Binary {
            operator: "<".to_owned(),
            left: Box::new(Form::Local(0)),
            right: Box::new(Form::Local(1)),
        };
        let form = pairwise(escaping(ordered, Form::Constant("false".to_owned())));
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::DecidesSomethingElse)
        );
    }

    /// Leaving with one of the elements means the pair is being used.
    ///
    /// Knowing that a duplicate exists does not tell you what it was, so an API
    /// that answers only the first question cannot stand in here.
    #[test]
    fn a_walk_that_escapes_with_an_element_is_refused() {
        let form = pairwise(escaping(equal_pair(), Form::Local(0)));
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::EscapesWithAValue)
        );
    }

    /// Skipping a duplicate pair is not testing for one.
    ///
    /// The commonest thing a pairwise equality test does in real code, by a
    /// wide margin, and it belongs to a larger computation rather than being
    /// one.
    #[test]
    fn a_walk_that_continues_past_a_duplicate_is_refused() {
        let form = pairwise(Form::Branch {
            condition: Box::new(equal_pair()),
            consequence: Box::new(Form::Opaque {
                kind: "continue_expression".to_owned(),
                parts: Vec::new(),
            }),
            alternative: None,
        });
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::NotThisShape)
        );
    }

    /// Counting duplicates asks how many, not whether.
    #[test]
    fn a_walk_that_counts_duplicates_is_refused() {
        let form = pairwise(Form::Branch {
            condition: Box::new(equal_pair()),
            consequence: Box::new(Form::Assign {
                operator: "+=".to_owned(),
                target: Box::new(Form::Local(2)),
                value: Box::new(Form::Number("1".to_owned())),
            }),
            alternative: None,
        });
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::NotThisShape)
        );
    }

    /// Setting a flag is the other way the check is written.
    #[test]
    fn a_walk_that_sets_a_flag_is_all_different() {
        let form = pairwise(Form::Branch {
            condition: Box::new(equal_pair()),
            consequence: Box::new(Form::Assign {
                operator: "=".to_owned(),
                target: Box::new(Form::Local(2)),
                value: Box::new(Form::Constant("false".to_owned())),
            }),
            alternative: None,
        });
        assert_eq!(
            all_different(&form, allowed()).map(|found| found.sequence),
            Ok(Form::Free(0))
        );
    }

    /// Recording and leaving the inner loop is one reaction written as two.
    ///
    /// `break` settles nothing by itself, but a walk whose flag is already set
    /// has nothing left to learn from the rest of the row.
    #[test]
    fn a_walk_that_records_then_breaks_is_all_different() {
        let form = pairwise(Form::Branch {
            condition: Box::new(equal_pair()),
            consequence: Box::new(Form::Sequence(vec![
                Form::Assign {
                    operator: "=".to_owned(),
                    target: Box::new(Form::Local(2)),
                    value: Box::new(Form::Constant("\"No\"".to_owned())),
                },
                Form::Opaque {
                    kind: "break_expression".to_owned(),
                    parts: Vec::new(),
                },
            ])),
            alternative: None,
        });
        assert_eq!(
            all_different(&form, allowed()).map(|found| found.sequence),
            Ok(Form::Free(0))
        );
    }

    /// A reaction that also does unaccounted work is refused.
    #[test]
    fn a_walk_that_does_more_than_record_is_refused() {
        let form = pairwise(Form::Branch {
            condition: Box::new(equal_pair()),
            consequence: Box::new(Form::Sequence(vec![
                Form::Assign {
                    operator: "=".to_owned(),
                    target: Box::new(Form::Local(2)),
                    value: Box::new(Form::Constant("false".to_owned())),
                },
                Form::Method {
                    name: "push".to_owned(),
                    receiver: Box::new(Form::Local(3)),
                    arguments: vec![Form::Number("1".to_owned())],
                },
            ])),
            alternative: None,
        });
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::NotThisShape)
        );
    }

    /// Reporting and leaving is one reaction written as two steps.
    ///
    /// This is the spelling CodeNet's submissions actually use.
    #[test]
    fn a_walk_that_reports_then_leaves_is_all_different() {
        let form = pairwise(Form::Branch {
            condition: Box::new(equal_pair()),
            consequence: Box::new(Form::Sequence(vec![
                Form::Opaque {
                    kind: "macro:println".to_owned(),
                    parts: vec![Form::Constant("\"no\"".to_owned())],
                },
                Form::Return(Box::new(Form::Literal)),
            ])),
            alternative: None,
        });
        assert_eq!(
            all_different(&form, allowed()).map(|found| found.sequence),
            Ok(Form::Free(0))
        );
    }

    /// A context with no allocator is told nothing rather than told wrongly.
    #[test]
    fn a_context_that_cannot_allocate_is_refused() {
        let form = pairwise(escaping(equal_pair(), Form::Constant("false".to_owned())));
        let context = Context {
            can_allocate: false,
        };
        assert_eq!(
            all_different(&form, context),
            Err(IdiomRefusal::CannotAllocate)
        );
    }

    /// The element bound is read off the catalog, not written from memory.
    #[test]
    fn the_recommendation_carries_the_catalogs_own_bound() {
        let conditions = Idiom::AllDifferent.conditions(&all_unique());
        assert!(conditions.contains(&Condition::Allocates));
        assert!(conditions.iter().any(|condition| matches!(
            condition,
            Condition::ElementBound { requires, code_requires }
                if requires == "Eq + Hash" && code_requires == "PartialEq"
        )));
    }

    /// A callable that no longer answers yes or no is not this API any more.
    #[test]
    fn a_callable_that_does_not_return_bool_is_not_a_predicate() {
        assert!(answers_a_predicate(&all_unique()));
        let mut changed = all_unique();
        if let Some(signature) = changed.signature.as_mut() {
            signature.output = Some(ExternalType::Primitive {
                name: "usize".to_owned(),
            });
        }
        assert!(!answers_a_predicate(&changed));
    }

    /// A catalog with no signature at all cannot be verified, so it is not used.
    #[test]
    fn a_callable_with_no_signature_is_not_a_predicate() {
        let mut unknown = all_unique();
        unknown.signature = None;
        assert!(!answers_a_predicate(&unknown));
        assert!(
            Idiom::AllDifferent
                .conditions(&unknown)
                .iter()
                .all(|condition| !matches!(condition, Condition::ElementBound { .. }))
        );
    }

    /// A sortedness check asks its question of neighbours, in order.
    #[test]
    fn an_ordered_test_over_neighbours_is_is_sorted() {
        let ordered = Form::Binary {
            operator: ">".to_owned(),
            left: Box::new(Form::Local(0)),
            right: Box::new(Form::Local(1)),
        };
        let form = adjacent(escaping(ordered, Form::Constant("false".to_owned())));
        assert_eq!(
            recognize(Idiom::IsSorted, &form, allowed()).map(|found| found.sequence),
            Ok(Form::Free(0))
        );
    }

    /// Reading the comparison backwards is the opposite question.
    #[test]
    fn a_reversed_ordering_is_not_is_sorted() {
        let reversed = Form::Binary {
            operator: ">".to_owned(),
            left: Box::new(Form::Local(1)),
            right: Box::new(Form::Local(0)),
        };
        let form = adjacent(escaping(reversed, Form::Constant("false".to_owned())));
        assert_eq!(
            recognize(Idiom::IsSorted, &form, allowed()),
            Err(IdiomRefusal::DecidesSomethingElse)
        );
    }

    /// Sortedness is a question about neighbours, not about every pair.
    ///
    /// `a > b` over every pair says every element is no greater than every
    /// later one, which is far more than sortedness and would be a much
    /// stronger claim to summarize as `is_sorted`.
    #[test]
    fn an_ordered_test_over_every_pair_is_not_is_sorted() {
        let ordered = Form::Binary {
            operator: ">".to_owned(),
            left: Box::new(Form::Local(0)),
            right: Box::new(Form::Local(1)),
        };
        let form = pairwise(escaping(ordered, Form::Constant("false".to_owned())));
        assert_eq!(
            recognize(Idiom::IsSorted, &form, allowed()),
            Err(IdiomRefusal::NotThisShape)
        );
    }

    /// Distinctness is not decided by a walk over neighbours.
    #[test]
    fn an_equality_test_over_neighbours_is_not_all_different() {
        let form = adjacent(escaping(equal_pair(), Form::Constant("false".to_owned())));
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::NotThisShape)
        );
    }

    /// A sortedness check allocates nothing, so a `const fn` may still have it.
    #[test]
    fn is_sorted_is_offered_where_nothing_may_allocate() {
        let ordered = Form::Binary {
            operator: ">".to_owned(),
            left: Box::new(Form::Local(0)),
            right: Box::new(Form::Local(1)),
        };
        let form = adjacent(escaping(ordered, Form::Constant("false".to_owned())));
        let context = Context {
            can_allocate: false,
        };
        assert!(recognize(Idiom::IsSorted, &form, context).is_ok());
    }

    /// Where both sides need the same of the elements there is nothing to check.
    #[test]
    fn a_bound_the_code_already_needs_is_not_a_condition() {
        let conditions = Idiom::IsSorted.conditions(&excerpted("slice-is-sorted"));
        assert!(
            !conditions
                .iter()
                .any(|condition| matches!(condition, Condition::ElementBound { .. })),
            "{conditions:?}"
        );
        assert_eq!(conditions, vec![Condition::IncomparableElements]);
    }

    fn adjacent(body: Form) -> Form {
        Form::Sequence(vec![
            Form::Pairwise {
                sequence: Box::new(Form::Free(0)),
                left: Box::new(Pattern::Binding(0)),
                right: Box::new(Pattern::Binding(1)),
                body: Box::new(body),
                coverage: Coverage::Adjacent,
            },
            Form::Constant("true".to_owned()),
        ])
    }

    /// One catalogued callable, copied out of a generated catalog.
    ///
    /// The standard library's catalog is not committed — 26 MB of generated
    /// JSON, reproducible from a rustup component — so a test that needs a real
    /// signature reads the excerpt rather than a pack that may not be built.
    fn excerpted(name: &str) -> ExternalCallable {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/catalog")
            .join(format!("{name}.json"));
        serde_json::from_slice(&std::fs::read(&path).expect("catalog excerpt"))
            .expect("parsing the excerpt")
    }

    /// A shipped catalog entry, read rather than written out.
    fn catalogued(path: &str) -> ExternalCallable {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infact-packs");
        for api in ["rust-itertools/api", "rust-std/api"] {
            let Ok(entries) = std::fs::read_dir(root.join(api)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(bytes) = std::fs::read(entry.path()) else {
                    continue;
                };
                let Ok(catalog) = serde_json::from_slice::<infact_core::ExternalCatalog>(&bytes)
                else {
                    continue;
                };
                if let Some(found) = catalog
                    .callables
                    .into_iter()
                    .find(|callable| callable.path == path)
                {
                    return found;
                }
            }
        }
        panic!("{path} is in no shipped catalog");
    }

    /// The catalog entry the recognizer points at, as it is shipped.
    fn all_unique() -> ExternalCallable {
        catalogued(Idiom::AllDifferent.callable_path().1)
    }
}
