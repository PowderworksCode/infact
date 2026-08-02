//! Deciding whether one form is an instance of another.
//!
//! A derived behavior is a pattern, not a literal: what a library takes as a
//! parameter is a hole, and any expression a caller writes there is an instance
//! of it. Matching is therefore unification rather than comparison, and it is
//! kept apart from the form definition because it is the part with state.

use crate::{Arm, Form, Pattern};

/// Resolves a pattern against a subject, remembering what each role stands for.
///
/// A pattern's free variables are holes that match any subterm; its locals must
/// line up with the subject's locals one-for-one. Both are recorded so that a
/// role used twice has to mean the same thing twice.
#[derive(Debug, Default, Clone)]
pub(crate) struct Bindings {
    holes: Vec<(u32, Form)>,
    locals: Vec<(u32, u32)>,
    /// Whether a traversal may do more than the pattern describes.
    fused: bool,
}

/// Whether a pattern is a hole applied to names, rather than a call to one.
fn applied_hole(pattern: &Form) -> bool {
    matches!(pattern, Form::Call { callee, arguments }
        if matches!(callee.as_ref(), Form::Free(_)) && !arguments.is_empty())
}

/// Whether a form stops or diverts iteration.
///
/// A traversal containing one of these does not visit every element, so it
/// cannot stand in for a library operation that does.
/// Macros that never return, so a branch ending in one produces no value.
///
/// `match x { Some(v) => v, None => panic!(..) }` is `expect`, not `unwrap_or`:
/// the second arm yields nothing a caller could have supplied. These are the
/// language's own diverging macros, so the list holds for every library.
const DIVERGING_MACROS: &[&str] = &[
    "macro:panic",
    "macro:unreachable",
    "macro:todo",
    "macro:unimplemented",
    "macro:abort",
    "macro:assert",
    "macro:assert_eq",
    "macro:assert_ne",
];

fn interrupts_iteration(form: &Form) -> bool {
    match form {
        Form::Opaque { kind, .. } => {
            matches!(
                kind.as_str(),
                "break_expression" | "continue_expression" | "try_expression"
            ) || DIVERGING_MACROS.contains(&kind.as_str())
        }
        Form::Return(_) => true,
        _ => form.children().into_iter().any(interrupts_iteration),
    }
}

impl Bindings {
    /// A fresh set of bindings, told whether to accept work done alongside the
    /// pattern rather than only the pattern itself.
    pub(crate) fn with_fusion(fused: bool) -> Self {
        Self {
            fused,
            ..Self::default()
        }
    }

    /// Match the remaining steps, stepping over statements that leave the
    /// behavior alone.
    pub(crate) fn follow(&mut self, haystack: &[Form], steps: &[Form]) -> bool {
        let Some((next, rest)) = steps.split_first() else {
            return true;
        };
        for (index, candidate) in haystack.iter().enumerate() {
            // once something touches what is already bound, the behavior is
            // entangled with it and no later position can undo that
            if haystack[..index].iter().any(|passed| self.touches(passed)) {
                return false;
            }
            let mut trial = self.clone();
            if trial.form(candidate, next) && trial.follow(&haystack[index + 1..], rest) {
                *self = trial;
                return true;
            }
        }
        false
    }

    /// The same walk, recording which positions were matched.
    pub(crate) fn follow_recording(
        &mut self,
        haystack: &[Form],
        steps: &[Form],
        offset: usize,
        matched: &mut Vec<usize>,
    ) -> bool {
        let Some((next, rest)) = steps.split_first() else {
            return true;
        };
        for (index, candidate) in haystack.iter().enumerate() {
            if haystack[..index].iter().any(|passed| self.touches(passed)) {
                return false;
            }
            let mut trial = self.clone();
            let mut found = matched.clone();
            found.push(offset + index);
            if trial.form(candidate, next)
                && trial.follow_recording(
                    &haystack[index + 1..],
                    rest,
                    offset + index + 1,
                    &mut found,
                )
            {
                *self = trial;
                *matched = found;
                return true;
            }
        }
        false
    }

    /// Whether a form mentions anything this match has bound.
    fn touches(&self, form: &Form) -> bool {
        self.locals
            .iter()
            .any(|(_, subject)| form.references_local(*subject))
    }

    fn bind_hole(&mut self, index: u32, subject: &Form) -> bool {
        match self.holes.iter().find(|(bound, _)| *bound == index) {
            Some((_, existing)) => existing == subject,
            None => {
                self.holes.push((index, subject.clone()));
                true
            }
        }
    }

    fn bind_local(&mut self, pattern: u32, subject: u32) -> bool {
        if let Some((_, bound)) = self.locals.iter().find(|(left, _)| *left == pattern) {
            return *bound == subject;
        }
        // a local is a one-to-one correspondence, so a subject local that is
        // already spoken for cannot stand for a second pattern local
        if self.locals.iter().any(|(_, bound)| *bound == subject) {
            return false;
        }
        self.locals.push((pattern, subject));
        true
    }

    fn pattern(&mut self, subject: &Pattern, pattern: &Pattern) -> bool {
        match (subject, pattern) {
            (Pattern::Binding(subject), Pattern::Binding(pattern)) => {
                self.bind_local(*pattern, *subject)
            }
            (Pattern::Ignored, Pattern::Ignored) => true,
            (
                Pattern::Variant {
                    name: subject_name,
                    parts: subject_parts,
                },
                Pattern::Variant {
                    name: pattern_name,
                    parts: pattern_parts,
                },
            ) => {
                subject_name == pattern_name
                    && subject_parts.len() == pattern_parts.len()
                    && subject_parts
                        .iter()
                        .zip(pattern_parts)
                        .all(|(subject, pattern)| self.pattern(subject, pattern))
            }
            (Pattern::Tuple(subject), Pattern::Tuple(pattern)) => {
                subject.len() == pattern.len()
                    && subject
                        .iter()
                        .zip(pattern)
                        .all(|(subject, pattern)| self.pattern(subject, pattern))
            }
            _ => false,
        }
    }

    /// Whether every arm the pattern names is decided the same way by the
    /// subject, in the canonical order both are held in.
    fn arms(&mut self, subject: &[Arm], pattern: &[Arm]) -> bool {
        self.arms_from(subject, pattern, &mut Vec::new())
    }

    /// Match each of the pattern's arms to a distinct arm of the subject.
    ///
    /// Both sides are sorted, but they do not line up: a library that names
    /// `None` and code that writes `_` in the same position sort differently,
    /// so this pairs arms rather than walking them in step.
    fn arms_from(&mut self, subject: &[Arm], pattern: &[Arm], taken: &mut Vec<usize>) -> bool {
        let Some((next, rest)) = pattern.split_first() else {
            return true;
        };
        for (index, candidate) in subject.iter().enumerate() {
            if taken.contains(&index) {
                continue;
            }
            // An arm that leaves the function is not an arm that produces a
            // value. `Err(_) => return` decides nothing a library could have
            // been given, so it must not stand for one that does.
            if interrupts_iteration(&candidate.body) != interrupts_iteration(&next.body) {
                continue;
            }
            let mut trial = self.clone();
            if trial.arm_pattern(&candidate.pattern, &next.pattern)
                && trial.form(&candidate.body, &next.body)
            {
                taken.push(index);
                if trial.arms_from(subject, rest, taken) {
                    *self = trial;
                    return true;
                }
                taken.pop();
            }
        }
        false
    }

    /// Whether a subject arm decides the case the pattern arm names.
    ///
    /// A catch-all covers every alternative not named beside it, so code that
    /// writes `_` where a library writes `None` is deciding the same case. This
    /// reads a subject's `_` as standing for what the library named — which is
    /// exact for a two-alternative type like `Option` and a generalization for
    /// anything wider.
    fn arm_pattern(&mut self, subject: &Pattern, pattern: &Pattern) -> bool {
        if matches!(subject, Pattern::Ignored) && matches!(pattern, Pattern::Variant { .. }) {
            return true;
        }
        self.pattern(subject, pattern)
    }

    fn all(&mut self, subject: &[Form], pattern: &[Form]) -> bool {
        subject.len() == pattern.len()
            && subject
                .iter()
                .zip(pattern)
                .all(|(subject, pattern)| self.form(subject, pattern))
    }

    /// Whether a traversal body performs the pattern's body among other steps.
    fn fused_body(&mut self, subject: &Form, pattern: &Form) -> bool {
        if !self.fused {
            return false;
        }
        let Form::Sequence(steps) = subject else {
            return false;
        };
        if steps.iter().any(interrupts_iteration) {
            return false;
        }
        steps.iter().any(|step| self.form(step, pattern))
    }

    pub(crate) fn form(&mut self, subject: &Form, pattern: &Form) -> bool {
        // a hole accepts any subterm, as long as it accepts the same one every
        // time it appears
        if let Form::Free(index) = pattern {
            return self.bind_hole(*index, subject);
        }
        if applied_hole(pattern) {
            // the structural reading is the more precise one, so it goes first
            let mut trial = self.clone();
            if trial.structural(subject, pattern) {
                *self = trial;
                return true;
            }
            return self.abstraction(subject, pattern);
        }
        self.structural(subject, pattern)
    }

    /// Whether a hole applied to bound names describes this subject.
    ///
    /// A library takes its predicate as an argument and applies it; the person
    /// who reimplements it writes the test out where the call would be. So
    /// `f(item)` in a derived behavior does not name a call — it names
    /// *whatever a caller does with the item*, and any expression over that item
    /// is an instance of it. Requiring the subject to mention every argument
    /// keeps this from accepting a test that ignores what it is iterating over.
    fn abstraction(&mut self, subject: &Form, pattern: &Form) -> bool {
        let Form::Call { callee, arguments } = pattern else {
            return false;
        };
        let Form::Free(hole) = callee.as_ref() else {
            return false;
        };
        // an argument standing for the whole expression says nothing
        if matches!(subject, Form::Local(_) | Form::Free(_)) {
            return false;
        }
        // A library applies the caller's function and uses what it returns. An
        // expression that can leave the loop instead — through `?`, `break`, or
        // a return of its own — is not a thing a caller could have passed in,
        // so the loop it sits in does not do what the library does.
        if interrupts_iteration(subject) {
            return false;
        }
        for argument in arguments {
            let Form::Local(index) = argument else {
                return false;
            };
            let Some((_, bound)) = self.locals.iter().find(|(left, _)| left == index) else {
                return false;
            };
            if !subject.references_local(*bound) {
                return false;
            }
        }
        self.bind_hole(*hole, subject)
    }

    fn structural(&mut self, subject: &Form, pattern: &Form) -> bool {
        match (subject, pattern) {
            (Form::Local(subject), Form::Local(pattern)) => self.bind_local(*pattern, *subject),
            (Form::Literal, Form::Literal) => true,
            (Form::Number(subject), Form::Number(pattern))
            | (Form::Constant(subject), Form::Constant(pattern)) => subject == pattern,
            (Form::Construct(subject), Form::Construct(pattern))
            | (Form::Path(subject), Form::Path(pattern)) => subject == pattern,
            (
                Form::Variant {
                    name: subject_name,
                    payload: subject_payload,
                },
                Form::Variant {
                    name: pattern_name,
                    payload: pattern_payload,
                },
            ) => subject_name == pattern_name && self.all(subject_payload, pattern_payload),
            (
                Form::Field {
                    value: subject,
                    name: subject_name,
                },
                Form::Field {
                    value: pattern,
                    name: pattern_name,
                },
            ) => subject_name == pattern_name && self.form(subject, pattern),
            (
                Form::Method {
                    name: subject_name,
                    receiver: subject_receiver,
                    arguments: subject_arguments,
                },
                Form::Method {
                    name: pattern_name,
                    receiver: pattern_receiver,
                    arguments: pattern_arguments,
                },
            ) => {
                subject_name == pattern_name
                    && self.form(subject_receiver, pattern_receiver)
                    && self.all(subject_arguments, pattern_arguments)
            }
            (
                Form::Call {
                    callee: subject_callee,
                    arguments: subject_arguments,
                },
                Form::Call {
                    callee: pattern_callee,
                    arguments: pattern_arguments,
                },
            ) => {
                self.form(subject_callee, pattern_callee)
                    && self.all(subject_arguments, pattern_arguments)
            }
            (
                Form::Traverse {
                    sequence: subject_sequence,
                    item: subject_item,
                    body: subject_body,
                    direction: subject_direction,
                },
                Form::Traverse {
                    sequence: pattern_sequence,
                    item: pattern_item,
                    body: pattern_body,
                    direction: pattern_direction,
                },
            ) => {
                // A search from the front does not answer a search from the
                // back. Everything else is what the other walks do, including
                // recognizing a body that does this work alongside other work.
                subject_direction == pattern_direction
                    && self.form(subject_sequence, pattern_sequence)
                    && self.pattern(subject_item, pattern_item)
                    && (self.form(subject_body, pattern_body)
                        || self.fused_body(subject_body, pattern_body))
            }
            (
                Form::Transform {
                    sequence: subject_sequence,
                    item: subject_item,
                    body: subject_body,
                },
                Form::Transform {
                    sequence: pattern_sequence,
                    item: pattern_item,
                    body: pattern_body,
                },
            )
            | (
                Form::Sift {
                    sequence: subject_sequence,
                    item: subject_item,
                    body: subject_body,
                },
                Form::Sift {
                    sequence: pattern_sequence,
                    item: pattern_item,
                    body: pattern_body,
                },
            )
            | (
                Form::Retain {
                    sequence: subject_sequence,
                    item: subject_item,
                    body: subject_body,
                },
                Form::Retain {
                    sequence: pattern_sequence,
                    item: pattern_item,
                    body: pattern_body,
                },
            ) => {
                self.form(subject_sequence, pattern_sequence)
                    && self.pattern(subject_item, pattern_item)
                    && (self.form(subject_body, pattern_body)
                        || self.fused_body(subject_body, pattern_body))
            }
            (
                Form::Accumulate {
                    sequence: subject_sequence,
                    initial: subject_initial,
                    accumulator: subject_accumulator,
                    item: subject_item,
                    body: subject_body,
                },
                Form::Accumulate {
                    sequence: pattern_sequence,
                    initial: pattern_initial,
                    accumulator: pattern_accumulator,
                    item: pattern_item,
                    body: pattern_body,
                },
            ) => {
                self.form(subject_sequence, pattern_sequence)
                    && self.form(subject_initial, pattern_initial)
                    && self.pattern(subject_accumulator, pattern_accumulator)
                    && self.pattern(subject_item, pattern_item)
                    && self.form(subject_body, pattern_body)
            }
            (
                Form::Collect {
                    sequence: subject_sequence,
                    container: subject_container,
                },
                Form::Collect {
                    sequence: pattern_sequence,
                    container: pattern_container,
                },
            ) => {
                // an inferred container matches a named one; the language, not
                // the author, decided whether the type had to be written down
                (subject_container == pattern_container
                    || subject_container.is_none()
                    || pattern_container.is_none())
                    && self.form(subject_sequence, pattern_sequence)
            }
            (
                Form::Assign {
                    operator: subject_operator,
                    target: subject_target,
                    value: subject_value,
                },
                Form::Assign {
                    operator: pattern_operator,
                    target: pattern_target,
                    value: pattern_value,
                },
            ) => {
                subject_operator == pattern_operator
                    && self.form(subject_target, pattern_target)
                    && self.form(subject_value, pattern_value)
            }
            (
                Form::Binary {
                    operator: subject_operator,
                    left: subject_left,
                    right: subject_right,
                },
                Form::Binary {
                    operator: pattern_operator,
                    left: pattern_left,
                    right: pattern_right,
                },
            ) => {
                subject_operator == pattern_operator
                    && self.form(subject_left, pattern_left)
                    && self.form(subject_right, pattern_right)
            }
            (
                Form::Lambda {
                    parameters: subject_parameters,
                    body: subject_body,
                },
                Form::Lambda {
                    parameters: pattern_parameters,
                    body: pattern_body,
                },
            ) => {
                subject_parameters.len() == pattern_parameters.len()
                    && subject_parameters
                        .iter()
                        .zip(pattern_parameters)
                        .all(|(subject, pattern)| self.pattern(subject, pattern))
                    && self.form(subject_body, pattern_body)
            }
            (
                Form::Let {
                    pattern: subject_binding,
                    value: subject_value,
                },
                Form::Let {
                    pattern: pattern_binding,
                    value: pattern_value,
                },
            ) => {
                self.form(subject_value, pattern_value)
                    && self.pattern(subject_binding, pattern_binding)
            }
            (
                Form::Branch {
                    condition: subject_condition,
                    consequence: subject_consequence,
                    alternative: subject_alternative,
                },
                Form::Branch {
                    condition: pattern_condition,
                    consequence: pattern_consequence,
                    alternative: pattern_alternative,
                },
            ) => {
                self.form(subject_condition, pattern_condition)
                    && self.form(subject_consequence, pattern_consequence)
                    && match (subject_alternative, pattern_alternative) {
                        (Some(subject), Some(pattern)) => self.form(subject, pattern),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (
                Form::Select {
                    scrutinee: subject_scrutinee,
                    arms: subject_arms,
                },
                Form::Select {
                    scrutinee: pattern_scrutinee,
                    arms: pattern_arms,
                },
            ) => {
                // Both sides are held sorted by what their arms name, so
                // comparing them in order is comparing them as sets. A subject
                // may decide more cases than the pattern does — code that also
                // handles a third variant still does what the library does for
                // the two it names — so the pattern's arms must be found among
                // the subject's rather than exhaust them.
                subject_arms.len() >= pattern_arms.len()
                    && self.form(subject_scrutinee, pattern_scrutinee)
                    && self.arms(subject_arms, pattern_arms)
            }
            (Form::Return(subject), Form::Return(pattern)) => self.form(subject, pattern),
            (Form::Sequence(subject), Form::Sequence(pattern)) => self.all(subject, pattern),
            (
                Form::Opaque {
                    kind: subject_kind,
                    parts: subject_parts,
                },
                Form::Opaque {
                    kind: pattern_kind,
                    parts: pattern_parts,
                },
            ) => subject_kind == pattern_kind && self.all(subject_parts, pattern_parts),
            _ => false,
        }
    }
}
