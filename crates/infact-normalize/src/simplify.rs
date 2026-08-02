//! An algebra of iteration.
//!
//! A library and its caller write the same behavior differently, and the gap is
//! not stylistic. `Iterator::find` is a fold over `ControlFlow` threaded through
//! a locally defined helper; the person who reimplements it writes a loop with
//! an early return. Comparing those as shapes fails, and compiling them does not
//! help — rustc's own inliner declines to fuse the fold, and LLVM unrolls the
//! two forms into different loops.
//!
//! What does bridge them is rewriting. Each law here is an equivalence that
//! holds in the language rather than in any library, so one set of them serves
//! every library there is:
//!
//! - **unfolding** — a name bound to a body is that body
//! - **traversal** — a fold whose accumulator is unused is a traversal
//! - **escape** — breaking out of a fold is returning from a loop
//! - **recovery** — asking a fold for the value it broke with is that return
//! - **generalization** — a name nothing in the form binds is a hole
//! - **fusion** — mapping what was filtered is one pass, not two
//!
//! Applied to a fixpoint, they carry both sides toward the same shape.

use crate::{Form, Pattern};

/// How many times to sweep before giving up.
///
/// The laws shrink a form or leave it alone, so a fixpoint is normally reached
/// in a few passes. The bound exists because unfolding can in principle keep
/// finding work, and a normalizer that fails to terminate is worse than one
/// that stops early.
const MAX_SWEEPS: usize = 8;

impl Form {
    /// Rewrite until no law applies.
    pub fn simplify(&self) -> Self {
        let mut current = self.clone();
        for _ in 0..MAX_SWEEPS {
            let next = current.sweep();
            if next == current {
                break;
            }
            current = next;
        }
        current.generalized()
    }

    /// A name with no binder left in the form stands for anything.
    ///
    /// Unfolding consumes binders: once `check`'s body has replaced the call to
    /// it, the predicate that was passed in is named by nothing. What remains is
    /// not a variable of this form but a parameter of it, which is what a hole
    /// is. Every caller supplies its own, so the form should accept any.
    fn generalized(&self) -> Self {
        let mut bound = Vec::new();
        self.collect_bindings(&mut bound);
        // holes and names are numbered separately, so a name promoted to a hole
        // needs a number no hole is already using, or it would silently become
        // the same hole as an unrelated one
        let mut fresh = Vec::new();
        let mut next = self.highest_hole().map_or(0, |highest| highest + 1);
        self.collect_unbound(&bound, &mut fresh);
        let renumbered = fresh
            .into_iter()
            .map(|index| {
                let assigned = next;
                next += 1;
                (index, assigned)
            })
            .collect::<Vec<_>>();
        self.with_free(&bound, &renumbered)
    }

    fn highest_hole(&self) -> Option<u32> {
        let here = match self {
            Self::Free(index) => Some(*index),
            _ => None,
        };
        self.children()
            .into_iter()
            .filter_map(|child| child.highest_hole())
            .chain(here)
            .max()
    }

    /// The names this form mentions but does not bind, in order of appearance.
    fn collect_unbound(&self, bound: &[u32], found: &mut Vec<u32>) {
        if let Self::Local(index) = self
            && !bound.contains(index)
            && !found.contains(index)
        {
            found.push(*index);
        }
        for child in self.children() {
            child.collect_unbound(bound, found);
        }
    }

    fn collect_bindings(&self, bound: &mut Vec<u32>) {
        for pattern in self.binders() {
            collect_pattern(pattern, bound);
        }
        for child in self.children() {
            child.collect_bindings(bound);
        }
    }

    /// The patterns this form introduces names with.
    fn binders(&self) -> Vec<&Pattern> {
        match self {
            Self::Traverse { item, .. }
            | Self::Transform { item, .. }
            | Self::Retain { item, .. } => vec![item],
            Self::Accumulate {
                accumulator, item, ..
            } => vec![accumulator, item],
            Self::Lambda { parameters, .. } => parameters.iter().collect(),
            Self::Let { pattern, .. } => vec![pattern],
            Self::Select { arms, .. } => arms.iter().map(|arm| &arm.pattern).collect(),
            Self::Sift { item, .. } => vec![item],
            _ => Vec::new(),
        }
    }

    fn with_free(&self, bound: &[u32], renumbered: &[(u32, u32)]) -> Self {
        match self {
            Self::Local(index) if !bound.contains(index) => Self::Free(
                renumbered
                    .iter()
                    .find(|(from, _)| from == index)
                    .map_or(*index, |(_, to)| *to),
            ),
            other => other.map_children(&|child| child.with_free(bound, renumbered)),
        }
    }

    /// One pass: rewrite the children, then this node.
    fn sweep(&self) -> Self {
        let rebuilt = self.map_children(&|child| child.sweep());
        rebuilt
            .as_fused()
            .or_else(|| rebuilt.as_escape())
            .or_else(|| rebuilt.as_traversal())
            .or_else(|| rebuilt.as_recovered_escape())
            .or_else(|| rebuilt.as_unfolded())
            .unwrap_or(rebuilt)
    }

    /// Mapping what was filtered is one pass that decides and produces.
    ///
    /// `filter(p).map(f)` visits twice only because that is how it is written;
    /// what it describes is `filter_map`. Fusing them is what lets code written
    /// as a chain compare against a library that offers the single operation —
    /// and against code written as a loop, which does it in one pass already.
    fn as_fused(&self) -> Option<Self> {
        let Self::Transform {
            sequence,
            item: mapped,
            body: produce,
        } = self
        else {
            return None;
        };
        let Self::Retain {
            sequence: source,
            item: tested,
            body: test,
        } = sequence.as_ref()
        else {
            return None;
        };
        // The two closures name the element separately; fusing them means the
        // second has to speak about the first's binding.
        let (Pattern::Binding(tested_index), Pattern::Binding(mapped_index)) =
            (tested.as_ref(), mapped.as_ref())
        else {
            return None;
        };
        let produced = produce.substitute(*mapped_index, &Self::Local(*tested_index));
        Some(Self::Sift {
            sequence: source.clone(),
            item: tested.clone(),
            body: Box::new(Self::Branch {
                condition: test.clone(),
                consequence: Box::new(Self::Variant {
                    name: "Some".to_owned(),
                    payload: vec![produced],
                }),
                alternative: Some(Box::new(Self::Variant {
                    name: "None".to_owned(),
                    payload: Vec::new(),
                })),
            }),
        })
    }

    /// Breaking out of a fold is returning from a loop, and continuing is
    /// doing nothing.
    ///
    /// `ControlFlow` is how a fold says what a loop says with `return` and
    /// falling through. They are the same control flow wearing different
    /// clothes, and the library wears one because it cannot write the other.
    fn as_escape(&self) -> Option<Self> {
        match self {
            Self::Variant { name, payload } if is_break(name) => {
                let value = payload.first().cloned().unwrap_or(Self::Literal);
                Some(Self::Return(Box::new(value)))
            }
            Self::Variant { name, .. } if is_continue(name) => Some(Self::Literal),
            // an alternative that does nothing is not an alternative
            Self::Branch {
                condition,
                consequence,
                alternative: Some(alternative),
            } if matches!(alternative.as_ref(), Self::Literal) => Some(Self::Branch {
                condition: condition.clone(),
                consequence: consequence.clone(),
                alternative: None,
            }),
            _ => None,
        }
    }

    /// A fold whose accumulator is never used is a traversal.
    ///
    /// `try_fold((), f)` walks the sequence applying `f`; the unit accumulator
    /// carries nothing, so what remains is a visit to each element.
    fn as_traversal(&self) -> Option<Self> {
        let Self::Method {
            name,
            receiver,
            arguments,
        } = self
        else {
            return None;
        };
        if !matches!(name.as_str(), "try_fold" | "try_for_each" | "fold") {
            return None;
        }
        let [initial, Self::Lambda { parameters, body }] = arguments.as_slice() else {
            return None;
        };
        // an accumulator that carries something is a reduction, not a traversal,
        // and `Accumulate` already describes that
        if !is_unit(initial) {
            return None;
        }
        let item = match parameters.as_slice() {
            [_accumulator, item] => item.clone(),
            [item] => item.clone(),
            _ => return None,
        };
        Some(Self::Traverse {
            sequence: receiver.clone(),
            item: Box::new(item),
            body: body.clone(),
        })
    }

    /// Asking a traversal for the value it broke with is returning that value.
    ///
    /// `break_value` turns a `ControlFlow` into an `Option`, so the traversal
    /// yields `Some` where it escaped and `None` where it ran out.
    fn as_recovered_escape(&self) -> Option<Self> {
        let Self::Method {
            name,
            receiver,
            arguments,
        } = self
        else {
            return None;
        };
        if name != "break_value" || !arguments.is_empty() {
            return None;
        }
        let Self::Traverse { .. } = receiver.as_ref() else {
            return None;
        };
        Some(Self::Sequence(vec![
            receiver.escapes_wrapped(),
            Self::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            },
        ]))
    }

    /// Rewrite a traversal's escapes to carry `Some`, as `break_value` does.
    fn escapes_wrapped(&self) -> Self {
        match self {
            Self::Return(value) => Self::Return(Box::new(Self::Variant {
                name: "Some".to_owned(),
                payload: vec![value.as_ref().clone()],
            })),
            other => other.map_children(&Self::escapes_wrapped),
        }
    }

    /// A call to a name bound to a lambda is that lambda's body.
    ///
    /// Only the outermost application is unfolded here; sweeping repeatedly
    /// reaches the rest.
    fn as_unfolded(&self) -> Option<Self> {
        let Self::Sequence(steps) = self else {
            return None;
        };
        let mut bindings = Vec::new();
        for step in steps {
            if let Self::Let { pattern, value } = step
                && let Pattern::Binding(index) = pattern.as_ref()
                && matches!(value.as_ref(), Self::Lambda { .. })
            {
                bindings.push((*index, value.as_ref().clone()));
            }
        }
        if bindings.is_empty() {
            return None;
        }
        let rewritten = steps
            .iter()
            .map(|step| step.apply_bindings(&bindings))
            .collect::<Vec<_>>();
        // a binding nothing refers to any more is noise
        let used = |index: u32| {
            rewritten.iter().any(|step| {
                !matches!(step, Self::Let { pattern, .. } if **pattern == Pattern::Binding(index))
                    && step.references_local(index)
            })
        };
        let kept = rewritten
            .iter()
            .filter(|step| match step {
                Self::Let { pattern, .. } => match pattern.as_ref() {
                    Pattern::Binding(index) => {
                        !bindings.iter().any(|(bound, _)| bound == index) || used(*index)
                    }
                    _ => true,
                },
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let simplified = if kept.len() == 1 {
            kept.into_iter().next().expect("one step")
        } else {
            Self::Sequence(kept)
        };
        (&simplified != self).then_some(simplified)
    }

    /// Replace calls to bound lambdas with their bodies.
    fn apply_bindings(&self, bindings: &[(u32, Self)]) -> Self {
        if let Self::Call { callee, arguments } = self
            && let Self::Local(index) = callee.as_ref()
            && let Some((_, Self::Lambda { parameters, body })) =
                bindings.iter().find(|(bound, _)| bound == index)
        {
            let mut substituted = body.as_ref().clone();
            for (parameter, argument) in parameters.iter().zip(arguments) {
                if let Pattern::Binding(bound) = parameter {
                    substituted = substituted.substitute(*bound, argument);
                }
            }
            return substituted.apply_bindings(bindings);
        }
        self.map_children(&|child| child.apply_bindings(bindings))
    }

    /// Replace a bound name with a value throughout.
    fn substitute(&self, index: u32, value: &Self) -> Self {
        match self {
            Self::Local(bound) if *bound == index => value.clone(),
            other => other.map_children(&|child| child.substitute(index, value)),
        }
    }
}

fn collect_pattern(pattern: &Pattern, bound: &mut Vec<u32>) {
    match pattern {
        Pattern::Binding(index) => bound.push(*index),
        Pattern::Tuple(parts) | Pattern::Variant { parts, .. } => {
            parts.iter().for_each(|part| collect_pattern(part, bound));
        }
        Pattern::Ignored => {}
    }
}

fn is_unit(form: &Form) -> bool {
    matches!(form, Form::Literal)
}

/// Whether a variant name is `ControlFlow`'s escape, however it is spelled.
fn is_break(name: &str) -> bool {
    name.rsplit("::").next() == Some("Break")
}

fn is_continue(name: &str) -> bool {
    name.rsplit("::").next() == Some("Continue")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lambda(parameter: u32, body: Form) -> Form {
        Form::Lambda {
            parameters: vec![Pattern::Binding(parameter)],
            body: Box::new(body),
        }
    }

    #[test]
    fn a_fold_with_nothing_to_accumulate_is_a_traversal() {
        let fold = Form::Method {
            name: "try_fold".to_owned(),
            receiver: Box::new(Form::Free(0)),
            arguments: vec![
                Form::Literal,
                Form::Lambda {
                    parameters: vec![Pattern::Ignored, Pattern::Binding(1)],
                    body: Box::new(Form::Local(1)),
                },
            ],
        };
        assert_eq!(
            fold.simplify().to_string(),
            "(traverse f0 v1 v1)",
            "a unit accumulator carries nothing"
        );
    }

    #[test]
    fn a_name_bound_to_a_body_is_that_body() {
        let sequence = Form::Sequence(vec![
            Form::Let {
                pattern: Box::new(Pattern::Binding(0)),
                value: Box::new(lambda(1, Form::Local(1))),
            },
            Form::Call {
                callee: Box::new(Form::Local(0)),
                arguments: vec![Form::Free(9)],
            },
        ]);
        // the call becomes the body with the argument in place, and the binding
        // nothing refers to any more disappears
        assert_eq!(sequence.simplify().to_string(), "f9");
    }

    #[test]
    fn recovering_a_break_value_yields_some_or_none() {
        let traversal = Form::Method {
            name: "break_value".to_owned(),
            receiver: Box::new(Form::Traverse {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(Form::Return(Box::new(Form::Local(1)))),
            }),
            arguments: Vec::new(),
        };
        assert_eq!(
            traversal.simplify().to_string(),
            "(do (traverse f0 v1 (return (variant Some v1))) (variant None))"
        );
    }

    #[test]
    fn breaking_out_of_a_fold_is_returning_from_a_loop() {
        let escape = Form::Branch {
            condition: Box::new(Form::Free(0)),
            consequence: Box::new(Form::Variant {
                name: "ControlFlow::Break".to_owned(),
                payload: vec![Form::Local(1)],
            }),
            alternative: Some(Box::new(Form::Variant {
                name: "ControlFlow::Continue".to_owned(),
                payload: vec![Form::Literal],
            })),
        };
        // the break becomes a return, and continuing becomes nothing at all;
        // nothing here binds the item, so it generalizes too
        assert_eq!(escape.simplify().to_string(), "(branch f0 (return f1))");
    }

    #[test]
    fn generalizing_does_not_collide_with_an_existing_hole() {
        let form = Form::Method {
            name: "apply".to_owned(),
            receiver: Box::new(Form::Free(0)),
            arguments: vec![Form::Local(0)],
        };
        // the receiver is already `f0`, so the promoted name cannot be
        assert_eq!(form.simplify().to_string(), "(method apply f0 f1)");
    }

    #[test]
    fn a_name_nothing_binds_becomes_a_hole() {
        let sequence = Form::Sequence(vec![
            Form::Let {
                pattern: Box::new(Pattern::Binding(0)),
                value: Box::new(lambda(1, Form::Local(1))),
            },
            Form::Call {
                callee: Box::new(Form::Local(0)),
                arguments: vec![Form::Local(7)],
            },
        ]);
        // `v7` is bound by nothing here, so it stands for whatever a caller
        // passes — under a number no existing hole has claimed
        assert_eq!(sequence.simplify().to_string(), "f0");
    }

    #[test]
    fn mapping_what_was_filtered_is_one_pass() {
        let chained = Form::Transform {
            sequence: Box::new(Form::Retain {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(Form::Method {
                    name: "is_ready".to_owned(),
                    receiver: Box::new(Form::Local(1)),
                    arguments: Vec::new(),
                }),
            }),
            item: Box::new(Pattern::Binding(2)),
            body: Box::new(Form::Method {
                name: "into_owned".to_owned(),
                receiver: Box::new(Form::Local(2)),
                arguments: Vec::new(),
            }),
        };
        // the two closures named the element separately; fusing makes the
        // second speak about the first's binding
        assert_eq!(
            chained.simplify().to_string(),
            "(sift f0 v1 (branch (method is_ready v1) \
             (variant Some (method into_owned v1)) (variant None)))"
        );
    }

    #[test]
    fn a_form_with_no_applicable_law_is_unchanged() {
        let plain = Form::Traverse {
            sequence: Box::new(Form::Free(0)),
            item: Box::new(Pattern::Binding(0)),
            body: Box::new(Form::Method {
                name: "push".to_owned(),
                receiver: Box::new(Form::Free(1)),
                arguments: vec![Form::Local(0)],
            }),
        };
        assert_eq!(plain.simplify(), plain);
    }

    #[test]
    fn simplifying_is_idempotent() {
        let fold = Form::Method {
            name: "fold".to_owned(),
            receiver: Box::new(Form::Free(0)),
            arguments: vec![Form::Literal, lambda(1, Form::Local(1))],
        };
        let once = fold.simplify();
        assert_eq!(once.simplify(), once);
    }
}
