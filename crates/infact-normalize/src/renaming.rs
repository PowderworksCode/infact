//! Renumbering roles so that alpha-equivalent forms compare equal.
//!
//! Which identifier an author chose is not behavior, so two forms that differ
//! only in the order names were introduced must reduce to the same thing. This
//! is a pure rewrite over an already-built form.

use crate::{Arm, Form, Pattern};

/// Renumbers roles by first appearance so that alpha-equivalent forms compare
/// equal. Locals and free variables are numbered independently, and a pattern
/// binding shares the numbering of the local it introduces.
#[derive(Debug, Default)]
pub(crate) struct Renaming {
    locals: Vec<u32>,
    frees: Vec<u32>,
}

impl Renaming {
    fn index(sequence: &mut Vec<u32>, original: u32) -> u32 {
        if let Some(position) = sequence.iter().position(|entry| *entry == original) {
            return u32::try_from(position).unwrap_or(u32::MAX);
        }
        sequence.push(original);
        u32::try_from(sequence.len() - 1).unwrap_or(u32::MAX)
    }

    fn pattern(&mut self, pattern: &Pattern) -> Pattern {
        match pattern {
            Pattern::Binding(index) => Pattern::Binding(Self::index(&mut self.locals, *index)),
            Pattern::Ignored => Pattern::Ignored,
            Pattern::Tuple(parts) => {
                Pattern::Tuple(parts.iter().map(|part| self.pattern(part)).collect())
            }
            Pattern::Variant { name, parts } => Pattern::Variant {
                name: name.clone(),
                parts: parts.iter().map(|part| self.pattern(part)).collect(),
            },
        }
    }

    fn boxed(&mut self, form: &Form) -> Box<Form> {
        Box::new(self.form(form))
    }

    /// Rewrite a form, visiting children in the order they are written so the
    /// numbering is deterministic.
    pub(crate) fn form(&mut self, form: &Form) -> Form {
        match form {
            Form::Local(index) => Form::Local(Self::index(&mut self.locals, *index)),
            Form::Free(index) => Form::Free(Self::index(&mut self.frees, *index)),
            Form::Literal => Form::Literal,
            Form::Number(value) => Form::Number(value.clone()),
            Form::Constant(value) => Form::Constant(value.clone()),
            Form::Construct(name) => Form::Construct(name.clone()),
            Form::Variant { name, payload } => Form::Variant {
                name: name.clone(),
                payload: payload.iter().map(|value| self.form(value)).collect(),
            },
            Form::Path(path) => Form::Path(path.clone()),
            Form::Field { value, name } => Form::Field {
                value: self.boxed(value),
                name: name.clone(),
            },
            Form::Method {
                name,
                receiver,
                arguments,
            } => Form::Method {
                name: name.clone(),
                receiver: self.boxed(receiver),
                arguments: arguments
                    .iter()
                    .map(|argument| self.form(argument))
                    .collect(),
            },
            Form::Call { callee, arguments } => Form::Call {
                callee: self.boxed(callee),
                arguments: arguments
                    .iter()
                    .map(|argument| self.form(argument))
                    .collect(),
            },
            Form::Traverse {
                sequence,
                item,
                body,
                direction,
            } => {
                let sequence = self.boxed(sequence);
                let item = Box::new(self.pattern(item));
                Form::Traverse {
                    sequence,
                    item,
                    body: self.boxed(body),
                    direction: *direction,
                }
            }
            Form::Pairwise {
                sequence,
                left,
                right,
                body,
                coverage,
            } => {
                let sequence = self.boxed(sequence);
                let left = Box::new(self.pattern(left));
                let right = Box::new(self.pattern(right));
                Form::Pairwise {
                    sequence,
                    left,
                    right,
                    body: self.boxed(body),
                    coverage: *coverage,
                }
            }
            Form::Sift {
                sequence,
                item,
                body,
            } => {
                let sequence = self.boxed(sequence);
                let item = Box::new(self.pattern(item));
                Form::Sift {
                    sequence,
                    item,
                    body: self.boxed(body),
                }
            }
            Form::Transform {
                sequence,
                item,
                body,
            } => {
                let sequence = self.boxed(sequence);
                let item = Box::new(self.pattern(item));
                Form::Transform {
                    sequence,
                    item,
                    body: self.boxed(body),
                }
            }
            Form::Retain {
                sequence,
                item,
                body,
            } => {
                let sequence = self.boxed(sequence);
                let item = Box::new(self.pattern(item));
                Form::Retain {
                    sequence,
                    item,
                    body: self.boxed(body),
                }
            }
            Form::Accumulate {
                sequence,
                initial,
                accumulator,
                item,
                body,
            } => {
                let sequence = self.boxed(sequence);
                let initial = self.boxed(initial);
                let accumulator = Box::new(self.pattern(accumulator));
                let item = Box::new(self.pattern(item));
                Form::Accumulate {
                    sequence,
                    initial,
                    accumulator,
                    item,
                    body: self.boxed(body),
                }
            }
            Form::Collect {
                sequence,
                container,
            } => Form::Collect {
                sequence: self.boxed(sequence),
                container: container.clone(),
            },
            Form::Assign {
                operator,
                target,
                value,
            } => Form::Assign {
                operator: operator.clone(),
                target: self.boxed(target),
                value: self.boxed(value),
            },
            Form::Binary {
                operator,
                left,
                right,
            } => Form::Binary {
                operator: operator.clone(),
                left: self.boxed(left),
                right: self.boxed(right),
            },
            Form::Repeat { condition, body } => Form::Repeat {
                condition: self.boxed(condition),
                body: self.boxed(body),
            },
            Form::Swap {
                sequence,
                left,
                right,
            } => Form::Swap {
                sequence: self.boxed(sequence),
                left: self.boxed(left),
                right: self.boxed(right),
            },
            Form::Unary { operator, value } => Form::Unary {
                operator: operator.clone(),
                value: self.boxed(value),
            },
            Form::Index { sequence, position } => Form::Index {
                sequence: self.boxed(sequence),
                position: self.boxed(position),
            },
            Form::Span {
                start,
                end,
                inclusive,
            } => Form::Span {
                start: self.boxed(start),
                end: self.boxed(end),
                inclusive: *inclusive,
            },
            Form::Lambda { parameters, body } => {
                let parameters = parameters
                    .iter()
                    .map(|parameter| self.pattern(parameter))
                    .collect();
                Form::Lambda {
                    parameters,
                    body: self.boxed(body),
                }
            }
            Form::Let { pattern, value } => {
                // the value is written before the name is bound
                let value = self.boxed(value);
                Form::Let {
                    pattern: Box::new(self.pattern(pattern)),
                    value,
                }
            }
            Form::Branch {
                condition,
                consequence,
                alternative,
            } => Form::Branch {
                condition: self.boxed(condition),
                consequence: self.boxed(consequence),
                alternative: alternative
                    .as_ref()
                    .map(|alternative| self.boxed(alternative)),
            },
            Form::Select { scrutinee, arms } => {
                let scrutinee = self.boxed(scrutinee);
                Form::Select {
                    scrutinee,
                    arms: arms
                        .iter()
                        .map(|arm| {
                            let pattern = self.pattern(&arm.pattern);
                            Arm {
                                pattern,
                                body: self.form(&arm.body),
                            }
                        })
                        .collect(),
                }
            }
            Form::Return(value) => Form::Return(self.boxed(value)),
            Form::Sequence(parts) => {
                Form::Sequence(parts.iter().map(|part| self.form(part)).collect())
            }
            Form::Opaque { kind, parts } => Form::Opaque {
                kind: kind.clone(),
                parts: parts.iter().map(|part| self.form(part)).collect(),
            },
        }
    }
}
