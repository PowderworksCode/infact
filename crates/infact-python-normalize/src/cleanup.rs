//! Passes over the form, once the syntax has been read.
//!
//! These name no node kind and touch no tree: they take the steps a body
//! produced and tidy what the reading could not. Kept apart from the reader
//! because they are a separate concern.

use infact_normalize::{Form, Pattern};

use crate::StatementSpan;

/// Containers a loop can be seen filling, and what a `Collect` should call them.
///
/// The method is what identifies the container, because the empty literal that
/// opened the loop has already reduced to a bare `Construct`. `add` belongs to
/// both `set` and `dict`; `set` is chosen because a dict is filled by
/// subscript assignment, which this does not recognize at all.
const FILLING_METHODS: &[(&str, &str)] = &[
    ("append", "list"),
    ("extend", "list"),
    ("add", "set"),
    ("update", "set"),
];

/// Add a step, splitting a statement that binds several names at once.
///
/// `a = b = compute()` and `a, b = pair` are each two bindings, and holding
/// them as one step hides both from the pass that drops what nothing reads.
/// They share the statement's span, because they share the statement.
pub(crate) fn push_flattened(
    steps: &mut Vec<(Form, StatementSpan)>,
    step: Form,
    span: StatementSpan,
) {
    if let Form::Sequence(parts) = &step
        && parts.iter().all(|part| matches!(part, Form::Let { .. }))
        && parts.len() > 1
    {
        let Form::Sequence(parts) = step else {
            return;
        };
        steps.extend(parts.into_iter().map(|part| (part, span)));
        return;
    }
    steps.push((step, span));
}

/// Replace a bound name with a value throughout a form.
fn substitute_local(form: &Form, index: u32, value: &Form) -> Form {
    match form {
        Form::Local(bound) if *bound == index => value.clone(),
        other => other.map_children(&|child| substitute_local(child, index, value)),
    }
}

/// Replace names bound to nothing but another name.
///
/// `self` is given a short local name constantly — `data = self.data` — and a
/// caller who already holds the value names it once. A form carrying the extra
/// binding could never match one written without it.
pub(crate) fn inline_aliases(steps: Vec<(Form, StatementSpan)>) -> Vec<(Form, StatementSpan)> {
    let mut kept: Vec<(Form, StatementSpan)> = Vec::new();
    let mut pending = steps;
    while !pending.is_empty() {
        let (step, span) = pending.remove(0);
        if let Form::Let { pattern, value } = &step
            && let Pattern::Binding(index) = pattern.as_ref()
            && matches!(value.as_ref(), Form::Free(_) | Form::Local(_))
        {
            pending = pending
                .iter()
                .map(|(later, at)| (substitute_local(later, *index, value), *at))
                .collect();
            continue;
        }
        kept.push((step, span));
    }
    kept
}

/// Drop `let` steps whose name nothing later refers to.
pub(crate) fn drop_unused_bindings(
    steps: Vec<(Form, StatementSpan)>,
) -> Vec<(Form, StatementSpan)> {
    let mut kept: Vec<(Form, StatementSpan)> = Vec::new();
    for (position, (step, span)) in steps.iter().enumerate() {
        let unused = matches!(step, Form::Let { pattern, .. }
            if matches!(pattern.as_ref(), Pattern::Binding(index)
                if !steps[position + 1..]
                    .iter()
                    .any(|(later, _)| later.references_local(*index))));
        if !unused {
            kept.push((step.clone(), *span));
        }
    }
    kept
}

/// A body's trailing `return` is what the body is worth.
///
/// Only the LAST step is touched: a `return` anywhere earlier is an escape from
/// the middle of the work, which is behavior and has to stay.
pub(crate) fn valued(form: Form) -> Form {
    match form {
        Form::Return(value) => *value,
        Form::Sequence(mut steps) => {
            if let Some(Form::Return(value)) = steps.last().cloned() {
                steps.pop();
                steps.push(*value);
            }
            Form::Sequence(steps)
        }
        other => other,
    }
}

/// The single most important rule: a loop that fills a container is a
/// comprehension.
///
/// Python spells "build a new sequence from an old one" three ways, and one of
/// them is three statements:
///
/// ```text
///   out = []                     [g(x) for x in xs if p(x)]
///   for x in xs:            ==
///       if p(x):
///           out.append(g(x))
/// ```
///
/// This is Python's version of the rule that made the TypeScript normalizer
/// worth anything — there, that an index walk and a `for..of` are one traversal.
/// Without it the two most common ways of writing the same thing reduce to
/// completely different forms and nothing else in this crate matters.
///
/// The rewrite runs over a body's steps rather than over one statement, because
/// the shape spans three of them: the empty container, the walk, and (usually)
/// the `return` that hands it back. It fires only when the container is filled
/// and never otherwise read inside the walk, so a loop that appends to
/// something it also inspects is left alone.
pub(crate) fn fuse_container_fills(
    steps: Vec<(Form, StatementSpan)>,
) -> Vec<(Form, StatementSpan)> {
    let mut kept: Vec<(Form, StatementSpan)> = Vec::new();
    let mut pending = steps;
    while !pending.is_empty() {
        let (step, span) = pending.remove(0);
        let Some(index) = empty_container_binding(&step) else {
            kept.push((step, span));
            continue;
        };
        let Some((walk, walk_span)) = pending.first().cloned() else {
            kept.push((step, span));
            continue;
        };
        let Some(collected) = collect_from_walk(&walk, index) else {
            kept.push((step, span));
            continue;
        };
        // Reading the container afterwards is fine — `return out` and
        // `print(out)` both just want the sequence, and substitution gives them
        // it. Sending it a message is not: `out.sort()` reorders a value this
        // rewrite would have folded into an expression, and there is nothing
        // left to reorder.
        if pending[1..]
            .iter()
            .any(|(later, _)| messages_local(later, index))
        {
            kept.push((step, span));
            continue;
        }
        pending.remove(0);
        // The container's own name is what a body returns at the end. Once the
        // walk IS the container, the name says nothing.
        pending = pending
            .iter()
            .map(|(later, at)| (substitute_local(later, index, &collected), *at))
            .collect();
        // Nothing reads it afterwards, so the value is the last thing the
        // sequence produces rather than a step in the middle of it.
        if pending.is_empty() {
            kept.push((collected, walk_span));
        }
    }
    kept
}

/// Whether a form calls a method on a local, or assigns through it.
///
/// The distinction that matters is between reading the container's value and
/// acting on the container itself. Only the second survives the rewrite as
/// something a caller could still do.
fn messages_local(form: &Form, index: u32) -> bool {
    let target = Form::Local(index);
    match form {
        Form::Method { receiver, .. } if receiver.as_ref() == &target => true,
        Form::Assign { target: to, .. } if to.as_ref() == &target => true,
        other => other
            .children()
            .into_iter()
            .any(|child| messages_local(child, index)),
    }
}

/// The local a step binds to an empty container, if that is what it does.
fn empty_container_binding(step: &Form) -> Option<u32> {
    let Form::Let { pattern, value } = step else {
        return None;
    };
    let Pattern::Binding(index) = pattern.as_ref() else {
        return None;
    };
    matches!(value.as_ref(), Form::Construct(_))
        .then_some(index)
        .copied()
}

/// A walk whose whole body fills `container`, as the sequence it produces.
fn collect_from_walk(walk: &Form, container: u32) -> Option<Form> {
    let Form::Traverse {
        sequence,
        item,
        body,
        direction,
    } = walk
    else {
        return None;
    };
    if !direction.is_forward() {
        return None;
    }
    let (kept, filled) = filling_body(body, container)?;
    // The element being appended is the transformed one; the condition, where
    // there is one, decides which elements are reached at all.
    let sequence = match kept {
        Some(condition) => Form::Retain {
            sequence: sequence.clone(),
            item: item.clone(),
            body: Box::new(condition),
        },
        None => *sequence.clone(),
    };
    let transformed = if item_is(item, &filled.value) {
        sequence
    } else {
        Form::Transform {
            sequence: Box::new(sequence),
            item: item.clone(),
            body: Box::new(filled.value),
        }
    };
    Some(Form::Collect {
        sequence: Box::new(transformed),
        container: Some(filled.container.to_owned()),
    })
}

/// What a walk body appends, and the condition guarding it.
struct Filled {
    value: Form,
    container: &'static str,
}

/// Read a walk's body as "append this, perhaps only when that".
///
/// Returns the guard, when the body is a single guarded append, alongside what
/// is appended. A body that does anything else is not a comprehension however
/// it is spelled.
fn filling_body(body: &Form, container: u32) -> Option<(Option<Form>, Filled)> {
    match body {
        Form::Branch {
            condition,
            consequence,
            alternative: None,
        } => {
            let (inner, filled) = filling_body(consequence, container)?;
            // `if a: if b: out.append(..)` is one condition written twice.
            let condition = match inner {
                Some(inner) => Form::Binary {
                    operator: "and".to_owned(),
                    left: condition.clone(),
                    right: Box::new(inner),
                },
                None => *condition.clone(),
            };
            Some((Some(condition), filled))
        }
        Form::Sequence(steps) => match steps.as_slice() {
            [only] => filling_body(only, container),
            _ => None,
        },
        Form::Method {
            name,
            receiver,
            arguments,
        } if receiver.as_ref() == &Form::Local(container) => {
            let container = FILLING_METHODS
                .iter()
                .find(|(method, _)| method == name)
                .map(|(_, container)| *container)?;
            let [value] = arguments.as_slice() else {
                return None;
            };
            // A body that also reads the container is not just filling it.
            if value.references_local(match receiver.as_ref() {
                Form::Local(index) => *index,
                _ => return None,
            }) {
                return None;
            }
            Some((
                None,
                Filled {
                    value: value.clone(),
                    container,
                },
            ))
        }
        _ => None,
    }
}

/// Whether a pattern binds exactly the local a form names.
///
/// `[x for x in xs]` transforms nothing, and saying it does would make a copy
/// look like work.
fn item_is(item: &Pattern, value: &Form) -> bool {
    matches!((item, value), (Pattern::Binding(bound), Form::Local(named)) if bound == named)
}
