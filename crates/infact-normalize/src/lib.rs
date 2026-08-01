//! A language-neutral normalized form for comparing program behavior.
//!
//! Two functions that do the same thing should reduce to the same [`Form`] even
//! when they are written differently: a loop against a combinator, a `HashMap`
//! built by `new` against one built by `with_hasher`, one author's `value`
//! against another's `item`. Normalizing both a library implementation and
//! repository code into this shape is what lets them be compared without any
//! per-library knowledge on either side.
//!
//! Each language supplies its own normalizer targeting this form. The form
//! itself names no language, library, or API.

use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

pub const NORMALIZED_FORM_SCHEMA: u32 = 1;

/// A name introduced by the code being normalized.
///
/// Only binding order survives normalization. Which identifier the author chose
/// is not behavior.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pattern {
    /// A binding, numbered by the order it was introduced.
    Binding(u32),
    /// A destructured tuple, such as a `(key, value)` loop pattern.
    Tuple(Vec<Pattern>),
    /// A binding the code discards.
    Ignored,
}

impl Display for Pattern {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(index) => write!(formatter, "v{index}"),
            Self::Ignored => formatter.write_str("_"),
            Self::Tuple(parts) => {
                formatter.write_str("(tuple")?;
                for part in parts {
                    write!(formatter, " {part}")?;
                }
                formatter.write_str(")")
            }
        }
    }
}

/// Normalized program behavior.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// A name bound by this code, by binding order.
    Local(u32),
    /// A name this code did not bind: a parameter, a field, a receiver. Free
    /// variables are interchangeable, so only their order of appearance counts.
    Free(u32),
    /// A literal whose value is not behavior: a string, a character, a boolean.
    /// That it is constant is what matters.
    Literal,
    /// A numeric literal, keeping its value.
    ///
    /// Numbers in accumulation are behavior, not noise: incrementing a counter
    /// by one is counting, and incrementing it by two is something else.
    Number(String),
    /// Construction of a named type, with the constructing function and its
    /// arguments discarded.
    Construct(String),
    /// A resolved path used as a value.
    Path(String),
    Field {
        value: Box<Form>,
        name: String,
    },
    Method {
        name: String,
        receiver: Box<Form>,
        arguments: Vec<Form>,
    },
    Call {
        callee: Box<Form>,
        arguments: Vec<Form>,
    },
    /// Visiting each element of a sequence. Every spelling of iteration in a
    /// language reduces to this, which is the single most important
    /// normalization the form performs.
    Traverse {
        sequence: Box<Form>,
        item: Box<Pattern>,
        body: Box<Form>,
    },
    /// Producing a new sequence by transforming each element.
    Transform {
        sequence: Box<Form>,
        item: Box<Pattern>,
        body: Box<Form>,
    },
    /// Producing a new sequence containing the elements that satisfy a test.
    Retain {
        sequence: Box<Form>,
        item: Box<Pattern>,
        body: Box<Form>,
    },
    /// Reducing a sequence to a single accumulated value.
    Accumulate {
        sequence: Box<Form>,
        initial: Box<Form>,
        accumulator: Box<Pattern>,
        item: Box<Pattern>,
        body: Box<Form>,
    },
    /// Gathering a sequence into a named container type.
    Collect {
        sequence: Box<Form>,
        container: Option<String>,
    },
    Assign {
        operator: String,
        target: Box<Form>,
        value: Box<Form>,
    },
    Binary {
        operator: String,
        left: Box<Form>,
        right: Box<Form>,
    },
    Lambda {
        parameters: Vec<Pattern>,
        body: Box<Form>,
    },
    Let {
        pattern: Box<Pattern>,
        value: Box<Form>,
    },
    Branch {
        condition: Box<Form>,
        consequence: Box<Form>,
        alternative: Option<Box<Form>>,
    },
    Return(Box<Form>),
    /// An ordered group of steps.
    Sequence(Vec<Form>),
    /// Syntax the normalizer has no canonical shape for yet. Retained so the
    /// surrounding behavior still compares, and so gaps in coverage stay
    /// visible rather than silently reducing unrelated code to the same form.
    Opaque {
        kind: String,
        parts: Vec<Form>,
    },
}

impl Form {
    /// Child forms, in evaluation order.
    pub fn children(&self) -> Vec<&Self> {
        match self {
            Self::Local(_)
            | Self::Free(_)
            | Self::Literal
            | Self::Number(_)
            | Self::Construct(_)
            | Self::Path(_) => Vec::new(),
            Self::Field { value, .. } | Self::Return(value) => vec![value.as_ref()],
            Self::Method {
                receiver,
                arguments,
                ..
            } => std::iter::once(receiver.as_ref())
                .chain(arguments.iter())
                .collect(),
            Self::Call { callee, arguments } => std::iter::once(callee.as_ref())
                .chain(arguments.iter())
                .collect(),
            Self::Traverse { sequence, body, .. }
            | Self::Transform { sequence, body, .. }
            | Self::Retain { sequence, body, .. } => vec![sequence.as_ref(), body.as_ref()],
            Self::Accumulate {
                sequence,
                initial,
                body,
                ..
            } => vec![sequence.as_ref(), initial.as_ref(), body.as_ref()],
            Self::Collect { sequence, .. } => vec![sequence.as_ref()],
            Self::Assign { target, value, .. } => vec![target.as_ref(), value.as_ref()],
            Self::Binary { left, right, .. } => vec![left.as_ref(), right.as_ref()],
            Self::Lambda { body, .. } => vec![body.as_ref()],
            Self::Let { value, .. } => vec![value.as_ref()],
            Self::Branch {
                condition,
                consequence,
                alternative,
            } => std::iter::once(condition.as_ref())
                .chain(std::iter::once(consequence.as_ref()))
                .chain(alternative.as_deref())
                .collect(),
            Self::Sequence(parts) | Self::Opaque { parts, .. } => parts.iter().collect(),
        }
    }

    /// How much behavior this form describes.
    ///
    /// Matching needs a significance floor: a form small enough to be a getter
    /// or a one-line delegation will collide across unrelated code, and
    /// reporting those collisions is noise rather than a finding.
    pub fn size(&self) -> u32 {
        1 + self
            .children()
            .iter()
            .map(|child| child.size())
            .sum::<u32>()
    }

    /// Renumber every role by its first appearance within this form.
    ///
    /// Role numbers are assigned while walking a whole function, so the same
    /// behavior carries different numbers depending on what surrounds it: an
    /// accumulator is `v0` in a function that declares nothing else and `v1` in
    /// one that declares a local first. Comparing behavior means comparing
    /// forms up to consistent renaming, and this puts any form into the
    /// representative shape that makes equality mean that.
    pub fn canonical(&self) -> Self {
        let mut renaming = Renaming::default();
        renaming.form(self)
    }

    /// Whether this form contains `pattern` anywhere within it.
    ///
    /// Repository code rarely consists of nothing but the behavior in question,
    /// so a match is a subtree relationship rather than whole-body equality.
    pub fn contains(&self, pattern: &Self) -> bool {
        !self.occurrences(pattern).is_empty()
    }

    /// Every position at which `pattern` occurs, outermost first.
    pub fn occurrences<'a>(&'a self, pattern: &Self) -> Vec<&'a Self> {
        let mut found = Vec::new();
        self.find(pattern, &mut found);
        if found.is_empty()
            && let Some(work) = pattern.without_result_reference()
        {
            self.find(&work, &mut found);
        }
        found
    }

    fn find<'a>(&'a self, pattern: &Self, found: &mut Vec<&'a Self>) {
        if self.matches(pattern) || self.contains_steps(pattern) {
            found.push(self);
        }
        for child in self.children() {
            child.find(pattern, found);
        }
    }

    /// The same behavior without its closing reference to what it built.
    ///
    /// A library function ends by naming its result, because it has to return
    /// it. Code that does the same work inline rarely does: it goes on to use
    /// the value, or returns something computed from it. That closing step is
    /// the function's obligation rather than part of the behavior, so matching
    /// looks for the work with and without it.
    pub fn without_result_reference(&self) -> Option<Self> {
        let Self::Sequence(steps) = self else {
            return None;
        };
        let (Self::Local(index), bound) = (steps.last()?, &steps[..steps.len() - 1]) else {
            return None;
        };
        if bound.len() < 2 {
            return None;
        }
        // only when this sequence is what bound the name, or the trailing
        // reference is to something from outside and cannot be dropped
        bound
            .iter()
            .any(|step| {
                matches!(step, Self::Let { pattern, .. }
                    if matches!(pattern.as_ref(), Pattern::Binding(bound) if bound == index))
            })
            .then(|| Self::Sequence(bound.to_vec()))
    }

    /// Whether this form is an instance of `pattern`.
    ///
    /// A derived behavior is a pattern, not a literal. What the library takes as
    /// a parameter is a hole: `sorted_by` receives its comparator from the
    /// caller, so any comparator matches, including a closure written inline.
    /// A hole that appears twice must be filled the same way both times.
    pub fn matches(&self, pattern: &Self) -> bool {
        let mut bindings = Bindings::default();
        bindings.form(self, pattern)
    }

    /// Whether a sequence of steps occurs contiguously inside this sequence.
    ///
    /// A behavior rarely arrives alone: repository code declares other locals
    /// around it and goes on to do more afterwards. The steps that make up the
    /// behavior still have to appear together and in order.
    fn contains_steps(&self, pattern: &Self) -> bool {
        let (Self::Sequence(haystack), Self::Sequence(steps)) = (self, pattern) else {
            return false;
        };
        if steps.is_empty() || steps.len() >= haystack.len() {
            return false;
        }
        haystack
            .windows(steps.len())
            .any(|window| Self::Sequence(window.to_vec()).matches(pattern))
    }

    /// Whether this form describes no behavior worth comparing.
    pub fn is_trivial(&self) -> bool {
        matches!(
            self,
            Self::Local(_) | Self::Free(_) | Self::Literal | Self::Number(_) | Self::Path(_)
        )
    }
}

impl Display for Form {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(index) => write!(formatter, "v{index}"),
            Self::Free(index) => write!(formatter, "f{index}"),
            Self::Literal => formatter.write_str("(lit)"),
            Self::Number(value) => write!(formatter, "(num {value})"),
            Self::Construct(name) => write!(formatter, "(construct {name})"),
            Self::Path(path) => write!(formatter, "(path {path})"),
            Self::Field { value, name } => write!(formatter, "(field {value} {name})"),
            Self::Method {
                name,
                receiver,
                arguments,
            } => {
                write!(formatter, "(method {name} {receiver}")?;
                for argument in arguments {
                    write!(formatter, " {argument}")?;
                }
                formatter.write_str(")")
            }
            Self::Call { callee, arguments } => {
                write!(formatter, "(call {callee}")?;
                for argument in arguments {
                    write!(formatter, " {argument}")?;
                }
                formatter.write_str(")")
            }
            Self::Traverse {
                sequence,
                item,
                body,
            } => write!(formatter, "(traverse {sequence} {item} {body})"),
            Self::Transform {
                sequence,
                item,
                body,
            } => write!(formatter, "(transform {sequence} {item} {body})"),
            Self::Retain {
                sequence,
                item,
                body,
            } => write!(formatter, "(retain {sequence} {item} {body})"),
            Self::Accumulate {
                sequence,
                initial,
                accumulator,
                item,
                body,
            } => write!(
                formatter,
                "(accumulate {sequence} {initial} {accumulator} {item} {body})"
            ),
            Self::Collect {
                sequence,
                container,
            } => match container {
                Some(container) => write!(formatter, "(collect {sequence} {container})"),
                None => write!(formatter, "(collect {sequence})"),
            },
            Self::Assign {
                operator,
                target,
                value,
            } => write!(formatter, "(assign {operator} {target} {value})"),
            Self::Binary {
                operator,
                left,
                right,
            } => write!(formatter, "(binary {operator} {left} {right})"),
            Self::Lambda { parameters, body } => {
                formatter.write_str("(lambda (")?;
                for (index, parameter) in parameters.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(" ")?;
                    }
                    write!(formatter, "{parameter}")?;
                }
                write!(formatter, ") {body})")
            }
            Self::Let { pattern, value } => write!(formatter, "(let {pattern} {value})"),
            Self::Branch {
                condition,
                consequence,
                alternative,
            } => match alternative {
                Some(alternative) => write!(
                    formatter,
                    "(branch {condition} {consequence} {alternative})"
                ),
                None => write!(formatter, "(branch {condition} {consequence})"),
            },
            Self::Return(value) => write!(formatter, "(return {value})"),
            Self::Sequence(parts) => {
                formatter.write_str("(do")?;
                for part in parts {
                    write!(formatter, " {part}")?;
                }
                formatter.write_str(")")
            }
            Self::Opaque { kind, parts } => {
                write!(formatter, "({kind}")?;
                for part in parts {
                    write!(formatter, " {part}")?;
                }
                formatter.write_str(")")
            }
        }
    }
}

/// Resolves a pattern against a subject, remembering what each role stands for.
///
/// A pattern's free variables are holes that match any subterm; its locals must
/// line up with the subject's locals one-for-one. Both are recorded so that a
/// role used twice has to mean the same thing twice.
#[derive(Debug, Default)]
struct Bindings {
    holes: Vec<(u32, Form)>,
    locals: Vec<(u32, u32)>,
}

impl Bindings {
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

    fn all(&mut self, subject: &[Form], pattern: &[Form]) -> bool {
        subject.len() == pattern.len()
            && subject
                .iter()
                .zip(pattern)
                .all(|(subject, pattern)| self.form(subject, pattern))
    }

    fn form(&mut self, subject: &Form, pattern: &Form) -> bool {
        // a hole accepts any subterm, as long as it accepts the same one every
        // time it appears
        if let Form::Free(index) = pattern {
            return self.bind_hole(*index, subject);
        }
        match (subject, pattern) {
            (Form::Local(subject), Form::Local(pattern)) => self.bind_local(*pattern, *subject),
            (Form::Literal, Form::Literal) => true,
            (Form::Number(subject), Form::Number(pattern)) => subject == pattern,
            (Form::Construct(subject), Form::Construct(pattern))
            | (Form::Path(subject), Form::Path(pattern)) => subject == pattern,
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
                },
                Form::Traverse {
                    sequence: pattern_sequence,
                    item: pattern_item,
                    body: pattern_body,
                },
            )
            | (
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
                    && self.form(subject_body, pattern_body)
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

/// Renumbers roles by first appearance so that alpha-equivalent forms compare
/// equal. Locals and free variables are numbered independently, and a pattern
/// binding shares the numbering of the local it introduces.
#[derive(Debug, Default)]
struct Renaming {
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
        }
    }

    fn boxed(&mut self, form: &Form) -> Box<Form> {
        Box::new(self.form(form))
    }

    /// Rewrite a form, visiting children in the order they are written so the
    /// numbering is deterministic.
    fn form(&mut self, form: &Form) -> Form {
        match form {
            Form::Local(index) => Form::Local(Self::index(&mut self.locals, *index)),
            Form::Free(index) => Form::Free(Self::index(&mut self.frees, *index)),
            Form::Literal => Form::Literal,
            Form::Number(value) => Form::Number(value.clone()),
            Form::Construct(name) => Form::Construct(name.clone()),
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
            } => {
                let sequence = self.boxed(sequence);
                let item = Box::new(self.pattern(item));
                Form::Traverse {
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

/// Assigns normalized roles to names as a language normalizer walks a body.
///
/// A name bound by the code becomes a local, numbered by binding order.
/// Anything else is free, numbered by first appearance.
#[derive(Debug, Default)]
pub struct Roles {
    roles: Vec<(String, Form)>,
    /// Names that hold values supplied from outside the body.
    values: Vec<String>,
    locals: u32,
    frees: u32,
}

impl Roles {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a name introduced by the code, shadowing any earlier role.
    pub fn bind(&mut self, name: &str) -> Form {
        let form = Form::Local(self.locals);
        self.locals += 1;
        self.roles.retain(|(existing, _)| existing != name);
        self.roles.push((name.to_owned(), form.clone()));
        form
    }

    /// Introduce a binding the source did not name.
    ///
    /// Some spellings leave a value implicit: passing a function to `map`
    /// describes the same traversal as passing a closure, but only the closure
    /// gives the item a name. Normalizing the two alike means supplying one.
    pub fn bind_anonymous(&mut self) -> Form {
        let form = Form::Local(self.locals);
        self.locals += 1;
        form
    }

    /// Record a name that holds a value supplied from outside the body: a
    /// parameter, or a receiver.
    ///
    /// It still resolves as a hole, because a caller may supply anything.
    /// Declaring it records only that the name is data rather than the name of
    /// a function, which is the difference between `f(x)` calling a parameter
    /// and `helper(x)` calling something defined elsewhere.
    pub fn declare(&mut self, name: &str) {
        if !self.values.iter().any(|existing| existing == name) {
            self.values.push(name.to_owned());
        }
    }

    /// Whether a name holds a value here, rather than naming a function defined
    /// outside this body.
    pub fn is_value(&self, name: &str) -> bool {
        self.values.iter().any(|existing| existing == name)
            || self
                .roles
                .iter()
                .any(|(existing, form)| existing == name && matches!(form, Form::Local(_)))
    }

    /// Resolve a name, treating anything not bound here as free.
    pub fn resolve(&mut self, name: &str) -> Form {
        if let Some((_, form)) = self
            .roles
            .iter()
            .rev()
            .find(|(existing, _)| existing == name)
        {
            return form.clone();
        }
        let form = Form::Free(self.frees);
        self.frees += 1;
        self.roles.push((name.to_owned(), form.clone()));
        form
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traverse() -> Form {
        Form::Traverse {
            sequence: Box::new(Form::Free(0)),
            item: Box::new(Pattern::Binding(1)),
            body: Box::new(Form::Method {
                name: "push".to_owned(),
                receiver: Box::new(Form::Local(0)),
                arguments: vec![Form::Local(1)],
            }),
        }
    }

    #[test]
    fn display_round_trips_through_serde() {
        let form = traverse();
        let json = serde_json::to_string(&form).unwrap();
        let parsed: Form = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, form);
        assert_eq!(form.to_string(), "(traverse f0 v1 (method push v0 v1))");
    }

    #[test]
    fn size_counts_every_node() {
        // traverse + sequence + method + receiver + argument
        assert_eq!(traverse().size(), 5);
        assert_eq!(Form::Literal.size(), 1);
    }

    #[test]
    fn a_behavior_is_found_inside_a_larger_body() {
        let body = Form::Sequence(vec![
            Form::Let {
                pattern: Box::new(Pattern::Binding(0)),
                value: Box::new(Form::Construct("Vec".to_owned())),
            },
            traverse(),
            Form::Local(0),
        ]);
        assert!(body.contains(&traverse()));
        assert_eq!(body.occurrences(&traverse()).len(), 1);
        assert!(!traverse().contains(&body));
    }

    #[test]
    fn locals_are_numbered_by_binding_order_and_free_names_by_appearance() {
        let mut roles = Roles::new();
        assert_eq!(roles.resolve("values"), Form::Free(0));
        assert_eq!(roles.bind("counts"), Form::Local(0));
        assert_eq!(roles.bind("value"), Form::Local(1));
        assert_eq!(roles.resolve("counts"), Form::Local(0));
        assert_eq!(roles.resolve("other"), Form::Free(1));
        // rebinding a name shadows the earlier role
        assert_eq!(roles.bind("value"), Form::Local(2));
        assert_eq!(roles.resolve("value"), Form::Local(2));
    }

    #[test]
    fn trivial_forms_are_recognized() {
        assert!(Form::Free(0).is_trivial());
        assert!(Form::Literal.is_trivial());
        assert!(!traverse().is_trivial());
    }
}
