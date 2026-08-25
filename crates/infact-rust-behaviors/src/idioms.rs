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

use infact_core::{Condition, ExternalBound, ExternalCallable, ExternalType, Form, Pattern};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Idiom {
    /// Deciding that no two elements of one sequence are equal.
    AllDifferent,
}

impl Idiom {
    /// The API this recommends, as a path into the package that offers it.
    pub const fn callable_path(&self) -> (&'static str, &'static str) {
        match self {
            Self::AllDifferent => ("itertools", "itertools::Itertools::all_unique"),
        }
    }

    /// What the code needs of its elements to compute this at all.
    ///
    /// The other half of an [`Condition::ElementBound`] comes from the catalog.
    /// This half is a property of the written-out shape: comparing two elements
    /// with `==` is all a pairwise distinctness check asks of them.
    const fn element_bound_of_the_code(&self) -> &'static str {
        match self {
            Self::AllDifferent => "PartialEq",
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
    /// to verify.
    pub fn conditions(&self, callable: &ExternalCallable) -> Vec<Condition> {
        let mut conditions = Vec::new();
        if let Some(requires) = element_bound(callable) {
            conditions.push(Condition::ElementBound {
                requires,
                code_requires: self.element_bound_of_the_code().to_owned(),
            });
        }
        match self {
            Self::AllDifferent => conditions.extend([
                Condition::Allocates,
                Condition::ComparisonObservable,
                Condition::SmallInputsFavourTheCode,
            ]),
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

/// Recognize an all-different check in a normalized function body.
///
/// The shape is a walk over each pair of one sequence that, on finding two
/// equal elements, does something that does not depend on WHICH pair it found.
/// That is the whole claim: a walk like that computes exactly whether a
/// duplicate exists, which is what the recommended API answers. What the code
/// then does with the answer — return it, print it, set a flag — is the
/// caller's business and does not change what the loop computed.
///
/// `Pairwise` is what makes this one shape rather than several: the index and
/// iterator spellings of the walk have already met by the time this runs.
///
/// Returns the sequence the check is over, which is what a caller would put the
/// recommended call on.
pub fn all_different(form: &Form, context: Context) -> Result<&Form, IdiomRefusal> {
    let sequence = deciding_pairwise(form)?;
    if !context.can_allocate {
        return Err(IdiomRefusal::CannotAllocate);
    }
    Ok(sequence)
}

/// Search a form for a pairwise walk that decides distinctness.
fn deciding_pairwise(form: &Form) -> Result<&Form, IdiomRefusal> {
    // Why the nearest thing to the shape was not it. A walk over pairs that
    // decides the wrong question is worth saying so about; not finding a walk
    // at all is the uninformative answer, so anything else outranks it.
    let mut refusal = IdiomRefusal::NotThisShape;
    match distinctness_walk(form) {
        Ok(sequence) => return Ok(sequence),
        Err(IdiomRefusal::NotThisShape) => {}
        Err(specific) => refusal = specific,
    }
    for child in form.children() {
        match deciding_pairwise(child) {
            Ok(sequence) => return Ok(sequence),
            Err(IdiomRefusal::NotThisShape) => {}
            Err(specific) => refusal = specific,
        }
    }
    Err(refusal)
}

/// A walk over pairs whose only reaction to an equal pair is to record that one
/// exists.
fn distinctness_walk(form: &Form) -> Result<&Form, IdiomRefusal> {
    let Form::Pairwise {
        sequence,
        left,
        right,
        body,
    } = form
    else {
        return Err(IdiomRefusal::NotThisShape);
    };
    let (Pattern::Binding(left), Pattern::Binding(right)) = (left.as_ref(), right.as_ref()) else {
        return Err(IdiomRefusal::NotThisShape);
    };
    // A test with an `else` is choosing between two things to do, and only one
    // of them is being described here.
    let Form::Branch {
        condition,
        consequence,
        alternative: None,
    } = body.as_ref()
    else {
        return Err(IdiomRefusal::NotThisShape);
    };
    if !compares_the_pair(condition, *left, *right) {
        return Err(IdiomRefusal::DecidesSomethingElse);
    }
    // Reading either element means the pair is being used for its content, and
    // an API that answers only whether a duplicate exists cannot supply that.
    if consequence.references_local(*left) || consequence.references_local(*right) {
        return Err(IdiomRefusal::EscapesWithAValue);
    }
    if !records_that_one_exists(consequence) {
        return Err(IdiomRefusal::NotThisShape);
    }
    Ok(sequence.as_ref())
}

/// Whether a reaction to an equal pair records only that one was found.
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
        // `println!("no"); return;` is one reaction written as two steps, and
        // it is the spelling the corpus actually uses. Only the last step may
        // leave, because a `return` reached in the middle would make the steps
        // after it dead rather than part of the reaction.
        Form::Sequence(steps) => steps.split_last().is_some_and(|(last, rest)| {
            records_that_one_exists(last) && !rest.iter().any(leaves_the_walk)
        }),
        _ => false,
    }
}

/// Whether a step can end the walk from somewhere other than its end.
fn leaves_the_walk(form: &Form) -> bool {
    matches!(form, Form::Return(_)) || form.children().into_iter().any(leaves_the_walk)
}

/// Whether a test asks whether the two elements of a pair are equal.
///
/// Equality only. `<` over every pair is a sortedness check and `!=` is the
/// opposite question, and both would be told to use a distinctness API by a
/// test that merely looked for a comparison.
fn compares_the_pair(condition: &Form, left: u32, right: u32) -> bool {
    let Form::Binary {
        operator,
        left: first,
        right: second,
    } = condition
    else {
        return false;
    };
    if operator != "==" {
        return false;
    }
    let named = |form: &Form, index: u32| *form == Form::Local(index);
    (named(first, left) && named(second, right)) || (named(first, right) && named(second, left))
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
        assert_eq!(all_different(&form, allowed()), Ok(&Form::Free(0)));
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
        assert_eq!(all_different(&form, allowed()), Ok(&Form::Free(0)));
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
        assert_eq!(all_different(&form, allowed()), Ok(&Form::Free(0)));
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

    /// The catalog entry the recognizer points at, as it is shipped.
    fn all_unique() -> ExternalCallable {
        let catalog = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../infact-packs/rust-itertools/api/itertools-0.15.0.json");
        let catalog: infact_core::ExternalCatalog =
            serde_json::from_slice(&std::fs::read(catalog).expect("itertools catalog"))
                .expect("parsing the catalog");
        catalog
            .callables
            .into_iter()
            .find(|callable| callable.path == Idiom::AllDifferent.callable_path().1)
            .expect("all_unique in the catalog")
    }
}
