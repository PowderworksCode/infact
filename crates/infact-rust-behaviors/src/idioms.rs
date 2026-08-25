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

use infact_core::{Condition, Form, Pattern};

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

    /// What has to hold for the recommendation to be sound.
    ///
    /// Fixed per idiom rather than per finding, because these are properties of
    /// the two implementations rather than of the code that was matched. They
    /// are stated in full even when several will usually be satisfied: a reader
    /// who can dismiss three of them in a second is better served than one who
    /// is not told about the fourth.
    pub fn conditions(&self) -> Vec<Condition> {
        match self {
            Self::AllDifferent => vec![
                Condition::ElementBound {
                    requires: "Eq + Hash".to_owned(),
                    code_requires: "PartialEq".to_owned(),
                },
                Condition::Allocates,
                Condition::ComparisonObservable,
                Condition::SmallInputsFavourTheCode,
            ],
        }
    }
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
/// The shape is a walk over each pair of one sequence that leaves with one
/// constant on finding an equal pair, in a body that yields the opposite
/// constant otherwise. `Pairwise` is what makes this one shape rather than
/// several: the index and iterator spellings of the walk have already met by
/// the time this runs.
///
/// Returns the sequence the check is over, which is what a caller would put the
/// recommended call on.
pub fn all_different(form: &Form, context: Context) -> Result<&Form, IdiomRefusal> {
    let (sequence, escaped, otherwise) = decisive_pairwise(form)?;
    // Two arms of one decision that name the same constant decide nothing.
    if escaped == otherwise {
        return Err(IdiomRefusal::NotThisShape);
    }
    if !context.can_allocate {
        return Err(IdiomRefusal::CannotAllocate);
    }
    Ok(sequence)
}

/// A pairwise walk that leaves with a constant, and the constant after it.
///
/// The walk and the value the body ends with are one claim: a walk that escapes
/// with `false` inside a body ending in `true` decides a question about every
/// pair, and the same walk followed by anything else is doing something this
/// cannot name.
fn decisive_pairwise(form: &Form) -> Result<(&Form, &str, &str), IdiomRefusal> {
    // Why the nearest thing to the shape was not it. A walk over pairs that
    // decides the wrong question is worth saying so about; not finding a walk
    // at all is the uninformative answer, so anything else outranks it.
    let mut refusal = IdiomRefusal::NotThisShape;
    if let Form::Sequence(steps) = form {
        // The walk need not be the first step: a function may bind or check
        // things before it. It must be immediately before the value, because
        // anything between them could change what is returned.
        for pair in steps.windows(2) {
            let [walk, Form::Constant(otherwise)] = pair else {
                continue;
            };
            match escaping_pairwise(walk) {
                Ok((sequence, escaped)) => return Ok((sequence, escaped, otherwise)),
                Err(IdiomRefusal::NotThisShape) => {}
                Err(specific) => refusal = specific,
            }
        }
    }
    for child in form.children() {
        match decisive_pairwise(child) {
            Ok(found) => return Ok(found),
            Err(IdiomRefusal::NotThisShape) => {}
            Err(specific) => refusal = specific,
        }
    }
    Err(refusal)
}

/// A walk over pairs that leaves as soon as two are equal.
fn escaping_pairwise(form: &Form) -> Result<(&Form, &str), IdiomRefusal> {
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
    let Form::Return(escaped) = consequence.as_ref() else {
        return Err(IdiomRefusal::NotThisShape);
    };
    let Form::Constant(escaped) = escaped.as_ref() else {
        return Err(IdiomRefusal::EscapesWithAValue);
    };
    Ok((sequence.as_ref(), escaped))
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

    /// Leaving with a computed value means the pairs are being used, not counted.
    #[test]
    fn a_walk_that_escapes_with_a_value_is_refused() {
        let form = pairwise(escaping(equal_pair(), Form::Local(0)));
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::EscapesWithAValue)
        );
    }

    /// Both arms naming one constant decides nothing.
    #[test]
    fn a_walk_that_yields_what_it_escapes_with_is_refused() {
        let form = pairwise(escaping(equal_pair(), Form::Constant("true".to_owned())));
        assert_eq!(
            all_different(&form, allowed()),
            Err(IdiomRefusal::NotThisShape)
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

    /// Every recommendation states what it depends on.
    #[test]
    fn the_recommendation_carries_its_conditions() {
        let conditions = Idiom::AllDifferent.conditions();
        assert!(conditions.contains(&Condition::Allocates));
        assert!(conditions.iter().any(|condition| matches!(
            condition,
            Condition::ElementBound { requires, .. } if requires == "Eq + Hash"
        )));
    }
}
