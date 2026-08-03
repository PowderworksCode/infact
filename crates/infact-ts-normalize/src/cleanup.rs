//! Passes over the form, once the syntax has been read.
//!
//! These name no node kind and touch no tree: they take the steps a body
//! produced and tidy what the reading could not. Kept apart from the reader
//! because they are a separate concern, and because the two together no longer
//! fit in one file.

use infact_normalize::{Form, Pattern};
use tree_sitter::Node;

use crate::StatementSpan;

/// Add a step, splitting a declaration that binds several names at once.
///
/// `var index = -1, length = xs.length;` is two bindings, and holding them as
/// one step hides both from the cleanup that drops what nothing reads — so a
/// counted loop's leftovers survive into the form and stop it comparing equal
/// to the same loop written another way. They share the statement's span,
/// because they share the statement.
pub(crate) fn push_flattened(
    steps: &mut Vec<(Form, StatementSpan)>,
    step: Form,
    span: StatementSpan,
    node: Node<'_>,
) {
    if matches!(node.kind(), "variable_declaration" | "lexical_declaration")
        && let Form::Sequence(parts) = step
    {
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
/// A specification implementation opens by giving its receiver a local name —
/// `var O = ToObject(this)` — and then speaks about `O`. Once the coercion is
/// recognized as identity, the binding says only that `O` *is* `this`, which is
/// a fact about spelling. A reimplementation names the same value once, so a
/// form carrying the extra binding could never match one written without it.
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

/// Replace the element access itself with the item a traversal binds.
///
/// A body that names the element first — `var v = a[k]` — speaks about `v`
/// afterwards, and binding `v` is enough. A body that does not, and writes
/// `a[k]` wherever it means the element, has to have those accesses rewritten
/// or the traversal binds an item nothing mentions. Most engine builtins are
/// written the second way.
pub(crate) fn replace_element_access(
    form: &Form,
    sequence: &Form,
    counter: &str,
    item: &Form,
) -> Form {
    if let Form::Field { value, name } = form
        && name == counter
        && value.as_ref() == sequence
    {
        return item.clone();
    }
    form.map_children(&|child| replace_element_access(child, sequence, counter, item))
}

/// Drop the arguments a traversal supplies from its own state.
///
/// The iteration protocol hands a callback the element, its index, and the
/// sequence: `predicate(kValue, k, O)`. Only the first is the value being
/// decided about — the other two are the traversal talking about itself, and
/// they are supplied by every caller of the protocol rather than written by
/// anyone. Code that reimplements the behavior writes `predicate(item)`, so
/// keeping them means the two never meet.
pub(crate) fn trim_protocol_arguments(form: &Form, item: &Form, sequence: &Form) -> Form {
    if let Form::Call { callee, arguments } = form
        && arguments.len() > 1
        && arguments.first() == Some(item)
        && arguments.last() == Some(sequence)
    {
        return Form::Call {
            callee: callee.clone(),
            arguments: vec![item.clone()],
        };
    }
    form.map_children(&|child| trim_protocol_arguments(child, item, sequence))
}

/// Drop `let` steps whose name nothing later refers to.
///
/// A specification implementation binds things a reimplementation never needs —
/// a `thisArg` pulled out of the argument list, a length read before the walk.
/// Once the operations that consumed them have been recognized as noise, the
/// bindings are left naming nothing, and a form carrying them cannot match one
/// written without them.
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
/// A library implements a search as a function and ends it `return undefined`;
/// a caller writes the same search as an expression and ends it with the value.
/// Only the LAST step is touched: a `return` anywhere earlier is an escape from
/// the middle of the work, which is behavior and has to stay.
pub(crate) fn valued(form: Form) -> Form {
    match form {
        Form::Return(value) => *value,
        Form::Sequence(mut steps) => {
            // the last step is inspected before it is removed: popping inside
            // the test would discard it whenever the pattern did not match,
            // which silently loses the statement the body was there for
            if let Some(Form::Return(value)) = steps.last().cloned() {
                steps.pop();
                steps.push(*value);
            }
            Form::Sequence(steps)
        }
        other => other,
    }
}

/// Whether a condition asks only whether an element is present.
pub(crate) fn is_presence_test(form: &Form) -> bool {
    matches!(form, Form::Binary { operator, .. } if operator == "in")
}
