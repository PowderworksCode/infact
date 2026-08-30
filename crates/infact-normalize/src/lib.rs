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

mod indexing;
mod matching;
mod renaming;
mod simplify;

pub use matching::Resolved;
use renaming::Renaming;

use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

pub const NORMALIZED_FORM_SCHEMA: u32 = 2;

/// The deepest a form may nest and still describe an operation.
///
/// A behavior anywhere near this describes a subsystem rather than an
/// operation.
pub const MAXIMUM_FORM_DEPTH: u32 = 32;

/// The smallest form worth reporting as a match.
///
/// Calibrated against derived behaviors and the code they are matched into.
/// The smallest genuine behavior measured is seven nodes, while the forms that
/// collide across unrelated code are two or three: a field accessor, a one-line
/// delegation, a struct literal. Anything below this floor describes too little
/// to identify an API.
pub const MINIMUM_REPORTABLE_SIZE: u32 = 6;

/// The least a behavior must name to identify an API rather than a shape.
///
/// Measured against the behaviors that matter: `sorted` names a container and a
/// method, which is two. A traversal that names nothing is every library's `map`
/// and matches everything.
pub const MINIMUM_ANCHORS: u32 = 2;

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
    /// A named alternative being taken apart, such as `Ok(value)`.
    ///
    /// Which variant an arm names is behavior — `Ok` and `Err` are the whole
    /// content of a decision about a `Result` — so unlike a binding, the name
    /// survives normalization.
    Variant { name: String, parts: Vec<Pattern> },
    /// A binding the code discards.
    Ignored,
}

impl Pattern {
    /// Whether this pattern introduces a particular binding.
    pub fn binds(&self, index: u32) -> bool {
        match self {
            Self::Binding(bound) => *bound == index,
            Self::Ignored => false,
            Self::Tuple(parts) | Self::Variant { parts, .. } => {
                parts.iter().any(|part| part.binds(index))
            }
        }
    }
}

impl Pattern {
    /// How much of a decision this pattern accounts for.
    pub fn size(&self) -> u32 {
        match self {
            Self::Binding(_) | Self::Ignored => 1,
            Self::Tuple(parts) => 1 + parts.iter().map(Self::size).sum::<u32>(),
            Self::Variant { parts, .. } => 1 + parts.iter().map(Self::size).sum::<u32>(),
        }
    }

    /// How much concrete named content this pattern carries.
    pub fn anchors(&self) -> u32 {
        match self {
            Self::Binding(_) | Self::Ignored => 0,
            Self::Tuple(parts) => parts.iter().map(Self::anchors).sum(),
            Self::Variant { parts, .. } => 1 + parts.iter().map(Self::anchors).sum::<u32>(),
        }
    }
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
            Self::Variant { name, parts } => {
                write!(formatter, "({name}")?;
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
    /// The unit value, and what an expression yielding nothing reduces to.
    Literal,
    /// A literal that is not a number: `true`, `"error"`, `'x'`.
    ///
    /// These used to reduce to `Literal` alongside `()`, on the reasoning that
    /// being constant was what mattered. It is not: `None => true` became
    /// indistinguishable from the `_ => ()` that an `if let` without an `else`
    /// produces, so every such `if let` looked like `Option::is_none_or` — 1,390
    /// times across five hundred crates. What a library returns is behavior,
    /// and nothing is exactly what it is not.
    Constant(String),
    /// A numeric literal, keeping its value.
    ///
    /// Numbers in accumulation are behavior, not noise: incrementing a counter
    /// by one is counting, and incrementing it by two is something else.
    Number(String),
    /// Construction of a named type, with the constructing function and its
    /// arguments discarded. `HashMap::new()` and `HashMap::with_capacity(8)`
    /// both make an empty map, and which one was written is not behavior.
    Construct(String),
    /// A named variant carrying a payload: `Some(x)`, `ControlFlow::Break(x)`.
    ///
    /// Unlike a constructor function, which variant was chosen *is* the
    /// behavior — `Break` and `Continue` are opposites — and the payload is the
    /// value being carried, not an argument to be discarded.
    Variant {
        name: String,
        payload: Vec<Form>,
    },
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
        /// Which way the walk runs.
        ///
        /// This lives inside the traversal rather than on the sequence because
        /// a pattern has to be able to see it. Expressed as a wrapper around
        /// the sequence — a reversal applied before walking — it would sit
        /// exactly where a derived behavior has a hole, and a hole absorbs
        /// anything: the forward search would match backwards code and report
        /// the opposite API. Held here, `find` and `findLast` cannot be
        /// mistaken for one another.
        ///
        /// A forward walk is not written out. Every behavior derived before
        /// this field existed is a forward walk, and pack contents are
        /// digested — emitting a default would change the digest of every
        /// published behavior that traverses anything while saying nothing new
        /// about it. Only a backwards walk is recorded, because only a
        /// backwards walk is news.
        #[serde(default, skip_serializing_if = "Direction::is_forward")]
        direction: Direction,
    },
    /// Visiting each unordered pair of a sequence's elements, once each.
    ///
    /// A nested loop over one collection is what a hand-rolled pairwise
    /// algorithm is made of, and the index arithmetic in its bounds is the
    /// whole statement of which pairs it visits. Left as two traversals, the
    /// spellings share nothing: `for i in 0..v.len() { for j in i+1.. }` and
    /// `v.iter().enumerate()` with `skip(i + 1)` have no subterm in common,
    /// though they visit the same pairs in the same order.
    ///
    /// This is the shape `itertools::tuple_combinations` offers, which is what
    /// earns it a place beside `Sift` and `Retain` rather than making it a
    /// special case: it is a normalization that lets a written-out loop compare
    /// against a library API that exists.
    ///
    /// Walking adjacent pairs is `windows(2)` and is not this: it is a third
    /// coverage, and it stays two traversals until something needs it.
    Pairwise {
        sequence: Box<Form>,
        left: Box<Pattern>,
        right: Box<Pattern>,
        body: Box<Form>,
        /// Which of the pairs the walk actually reaches.
        ///
        /// Written out, the two are a triangular nested loop and a square one
        /// with the diagonal guarded away, and both are common. They reach the
        /// same pairs and differ in how often, which is behavior: a decision
        /// that does not care how many times it sees a pair gets the same
        /// answer from either, and a count gets double. Recording it is what
        /// lets a reader of the form tell which they have.
        coverage: Coverage,
    },
    /// Repeating a body for as long as a condition holds.
    ///
    /// A `while` and a `loop` are one construct: the second is the first with
    /// the condition written inside as a `break`. Held as syntax they were two,
    /// and neither described work, so a library function that loops this way
    /// yielded no behavior at all.
    ///
    /// Distinct from [`Form::Traverse`], which walks something. A repetition
    /// has no sequence — what it visits, if anything, is whatever its body
    /// advances. Where that is a counted index, simplification turns the whole
    /// thing into the traversal it is written to be.
    Repeat {
        condition: Box<Form>,
        body: Box<Form>,
    },
    /// Exchanging two of a sequence's elements.
    ///
    /// `v.swap(i, j)` and the three-line dance through a temporary are the same
    /// operation, and held apart they share no subterm. This is the operation
    /// every naive sort is built from, so a form that cannot say it cannot say
    /// anything about one.
    Swap {
        sequence: Box<Form>,
        left: Box<Form>,
        right: Box<Form>,
    },
    /// Producing a new sequence by transforming each element.
    Transform {
        sequence: Box<Form>,
        item: Box<Pattern>,
        body: Box<Form>,
    },
    /// Producing a new sequence from the elements that yield a value.
    ///
    /// This is `filter_map`, and it is the fused form of a `Retain` followed by
    /// a `Transform`: deciding and producing in one pass. Keeping it apart from
    /// `Transform` matters because `map` cannot drop an element and this can.
    Sift {
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
    /// An operator applied to one value: `!found`, `-count`.
    ///
    /// Negation used to be stripped as noise alongside parentheses and `&`,
    /// which made a predicate and its opposite the same form: `if !seen.insert(x)`
    /// and `if seen.insert(x)` both reduced to `(branch (method insert ..) ..)`.
    /// A library that returns on one and a caller that returns on the other do
    /// opposite things, so the operator is behavior. A dereference genuinely is
    /// noise and is still stripped by the frontend, which is why this records
    /// the operator rather than the syntax it came from.
    Unary {
        operator: String,
        value: Box<Form>,
    },
    /// Reading a sequence at a position.
    ///
    /// Held apart from `Opaque` because the two parts have roles: which one is
    /// the sequence and which is the position is the whole content of an
    /// indexing, and `Opaque` compares parts by position alone.
    Index {
        sequence: Box<Form>,
        position: Box<Form>,
    },
    /// A span of integers, as a loop over indices walks.
    ///
    /// `inclusive` is carried rather than folded into `end`, because folding
    /// would need arithmetic on an expression that is usually symbolic. Left in
    /// `Opaque`, `0..n` and `0..=n` had the same kind and the same arity and
    /// unified with each other, which is an off-by-one reported as a match.
    Span {
        start: Box<Form>,
        end: Box<Form>,
        inclusive: bool,
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
    /// Choosing among named alternatives.
    ///
    /// A `match` is how a library takes apart an `Option`, a `Result`, or any
    /// enum, and it is the most common construct in the standard library. It is
    /// held with its arms sorted by what they name, because `Some` before
    /// `None` and `None` before `Some` are the same decision written two ways,
    /// and comparing them as written would make the order into behavior.
    Select {
        scrutinee: Box<Form>,
        arms: Vec<Arm>,
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

/// How often a pairwise walk reaches each pair.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Coverage {
    /// Each unordered pair once: `for i in 0..n { for j in i + 1..n { .. } }`,
    /// which is what `itertools::tuple_combinations` offers.
    #[default]
    Once,
    /// Each unordered pair both ways round, and no element with itself.
    ///
    /// A square nested loop over one sequence with an `i != j` guard. Every
    /// pair is visited twice, in both orders, so this says strictly less than
    /// [`Coverage::Once`] about anything order- or count-sensitive and exactly
    /// as much about anything that is neither.
    BothWays,
    /// Each element with the one after it: the `windows(2)` walk.
    ///
    /// A single loop, not a nested one, and the only coverage where `left` and
    /// `right` are neighbours rather than an arbitrary pair. That makes it the
    /// one that says something about ORDER: a test over adjacent pairs decides
    /// whether the sequence is sorted, and the same test over every pair
    /// decides something much stronger.
    Adjacent,
}

/// Whether a position is one past a named one.
///
/// Canonicalized arithmetic puts the name first, but both orders are accepted
/// so that a rewrite does not depend on which way the ordering happened to fall.
fn is_next_position(form: &Form, index: u32) -> bool {
    matches!(form, Form::Binary { operator, left, right }
        if operator == "+"
            && ((**left == Form::Local(index) && **right == Form::Number("1".to_owned()))
                || (**right == Form::Local(index) && **left == Form::Number("1".to_owned()))))
}

/// Which way a walk runs.
///
/// Only a walk that can stop early is changed by this: searching from the front
/// and searching from the back are different questions with different answers.
/// A walk that visits everything reaches the same elements either way.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// From the first element toward the last.
    #[default]
    Forward,
    /// From the last element toward the first.
    Backward,
}

impl Direction {
    /// Whether this is the direction a walk runs unless it says otherwise.
    #[must_use]
    pub const fn is_forward(&self) -> bool {
        matches!(self, Self::Forward)
    }
}

/// One alternative of a `Select`, and what it evaluates to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Arm {
    pub pattern: Pattern,
    pub body: Form,
}

impl Arm {
    /// What this arm names, for putting arms in a canonical order.
    ///
    /// An arm that names a variant sorts by that name; one that binds or
    /// discards matches anything and so has to come last, where a catch-all
    /// belongs.
    fn order(&self) -> (u8, &str) {
        match &self.pattern {
            Pattern::Variant { name, .. } => (0, name.as_str()),
            _ => (1, ""),
        }
    }
}

impl Form {
    /// A `Select` with its arms in canonical order.
    ///
    /// Sorting is stable, so arms that name the same thing keep the order they
    /// were written in and nothing is silently reordered past a duplicate.
    pub fn select(scrutinee: Self, mut arms: Vec<Arm>) -> Self {
        arms.sort_by(|left, right| left.order().cmp(&right.order()));
        Self::Select {
            scrutinee: Box::new(scrutinee),
            arms,
        }
    }

    /// Child forms, in evaluation order.
    pub fn children(&self) -> Vec<&Self> {
        match self {
            Self::Local(_)
            | Self::Free(_)
            | Self::Literal
            | Self::Constant(_)
            | Self::Number(_)
            | Self::Construct(_)
            | Self::Path(_) => Vec::new(),
            Self::Variant { payload, .. } => payload.iter().collect(),
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
            | Self::Sift { sequence, body, .. }
            | Self::Transform { sequence, body, .. }
            | Self::Pairwise { sequence, body, .. }
            | Self::Retain { sequence, body, .. } => vec![sequence.as_ref(), body.as_ref()],
            Self::Accumulate {
                sequence,
                initial,
                body,
                ..
            } => vec![sequence.as_ref(), initial.as_ref(), body.as_ref()],
            Self::Collect { sequence, .. } => vec![sequence.as_ref()],
            Self::Assign { target, value, .. } => vec![target.as_ref(), value.as_ref()],
            Self::Repeat { condition, body } => vec![condition.as_ref(), body.as_ref()],
            Self::Swap {
                sequence,
                left,
                right,
            } => vec![sequence.as_ref(), left.as_ref(), right.as_ref()],
            Self::Binary { left, right, .. } => vec![left.as_ref(), right.as_ref()],
            Self::Unary { value, .. } => vec![value.as_ref()],
            Self::Index { sequence, position } => vec![sequence.as_ref(), position.as_ref()],
            Self::Span { start, end, .. } => vec![start.as_ref(), end.as_ref()],
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
            Self::Select { scrutinee, arms } => std::iter::once(scrutinee.as_ref())
                .chain(arms.iter().map(|arm| &arm.body))
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
        // What a decision's arms name is part of what it describes, and it is
        // not reachable through the child forms.
        let named = match self {
            Self::Select { arms, .. } => arms.iter().map(|arm| arm.pattern.size()).sum(),
            _ => 0,
        };
        1 + named
            + self
                .children()
                .iter()
                .map(|child| child.size())
                .sum::<u32>()
    }

    /// Whether this form mentions a particular local.
    pub fn references_local(&self, index: u32) -> bool {
        match self {
            Self::Local(bound) => *bound == index,
            Self::Let { pattern, value } => pattern.binds(index) || value.references_local(index),
            Self::Traverse { item, .. }
            | Self::Sift { item, .. }
            | Self::Transform { item, .. }
            | Self::Retain { item, .. }
                if item.binds(index) =>
            {
                true
            }
            Self::Pairwise { left, right, .. } if left.binds(index) || right.binds(index) => true,
            _ => self
                .children()
                .into_iter()
                .any(|child| child.references_local(index)),
        }
    }

    /// Rebuild this form with every child passed through `transform`.
    ///
    /// Rewriting is mostly recursion, and writing that recursion once here
    /// keeps each law to the case it actually cares about.
    pub fn map_children(&self, transform: &dyn Fn(&Self) -> Self) -> Self {
        let apply = |form: &Self| Box::new(transform(form));
        let each = |forms: &[Self]| forms.iter().map(transform).collect::<Vec<_>>();
        match self {
            Self::Local(_)
            | Self::Free(_)
            | Self::Literal
            | Self::Constant(_)
            | Self::Number(_)
            | Self::Construct(_)
            | Self::Path(_) => self.clone(),
            Self::Variant { name, payload } => Self::Variant {
                name: name.clone(),
                payload: each(payload),
            },
            Self::Field { value, name } => Self::Field {
                value: apply(value),
                name: name.clone(),
            },
            Self::Method {
                name,
                receiver,
                arguments,
            } => Self::Method {
                name: name.clone(),
                receiver: apply(receiver),
                arguments: each(arguments),
            },
            Self::Call { callee, arguments } => Self::Call {
                callee: apply(callee),
                arguments: each(arguments),
            },
            Self::Traverse {
                sequence,
                item,
                body,
                direction,
            } => Self::Traverse {
                sequence: apply(sequence),
                item: item.clone(),
                body: apply(body),
                direction: *direction,
            },
            Self::Sift {
                sequence,
                item,
                body,
            } => Self::Sift {
                sequence: apply(sequence),
                item: item.clone(),
                body: apply(body),
            },
            Self::Pairwise {
                sequence,
                left,
                right,
                body,
                coverage,
            } => Self::Pairwise {
                sequence: apply(sequence),
                left: left.clone(),
                right: right.clone(),
                body: apply(body),
                coverage: *coverage,
            },
            Self::Transform {
                sequence,
                item,
                body,
            } => Self::Transform {
                sequence: apply(sequence),
                item: item.clone(),
                body: apply(body),
            },
            Self::Retain {
                sequence,
                item,
                body,
            } => Self::Retain {
                sequence: apply(sequence),
                item: item.clone(),
                body: apply(body),
            },
            Self::Accumulate {
                sequence,
                initial,
                accumulator,
                item,
                body,
            } => Self::Accumulate {
                sequence: apply(sequence),
                initial: apply(initial),
                accumulator: accumulator.clone(),
                item: item.clone(),
                body: apply(body),
            },
            Self::Collect {
                sequence,
                container,
            } => Self::Collect {
                sequence: apply(sequence),
                container: container.clone(),
            },
            Self::Assign {
                operator,
                target,
                value,
            } => Self::Assign {
                operator: operator.clone(),
                target: apply(target),
                value: apply(value),
            },
            Self::Binary {
                operator,
                left,
                right,
            } => Self::Binary {
                operator: operator.clone(),
                left: apply(left),
                right: apply(right),
            },
            Self::Repeat { condition, body } => Self::Repeat {
                condition: apply(condition),
                body: apply(body),
            },
            Self::Swap {
                sequence,
                left,
                right,
            } => Self::Swap {
                sequence: apply(sequence),
                left: apply(left),
                right: apply(right),
            },
            Self::Unary { operator, value } => Self::Unary {
                operator: operator.clone(),
                value: apply(value),
            },
            Self::Index { sequence, position } => Self::Index {
                sequence: apply(sequence),
                position: apply(position),
            },
            Self::Span {
                start,
                end,
                inclusive,
            } => Self::Span {
                start: apply(start),
                end: apply(end),
                inclusive: *inclusive,
            },
            Self::Lambda { parameters, body } => Self::Lambda {
                parameters: parameters.clone(),
                body: apply(body),
            },
            Self::Let { pattern, value } => Self::Let {
                pattern: pattern.clone(),
                value: apply(value),
            },
            Self::Branch {
                condition,
                consequence,
                alternative,
            } => Self::Branch {
                condition: apply(condition),
                consequence: apply(consequence),
                alternative: alternative.as_ref().map(|form| apply(form)),
            },
            Self::Select { scrutinee, arms } => Self::Select {
                scrutinee: apply(scrutinee),
                arms: arms
                    .iter()
                    .map(|arm| Arm {
                        pattern: arm.pattern.clone(),
                        body: transform(&arm.body),
                    })
                    .collect(),
            },
            Self::Return(value) => Self::Return(apply(value)),
            Self::Sequence(parts) => Self::Sequence(each(parts)),
            Self::Opaque { kind, parts } => Self::Opaque {
                kind: kind.clone(),
                parts: each(parts),
            },
        }
    }

    /// How much of this form is specific rather than structural.
    ///
    /// Structure alone does not identify an API. Every library has a `map`, and
    /// they all reduce to the same traversal over holes, so a form built only
    /// from shape matches any code of that shape. What distinguishes a behavior
    /// is what it names: a type it constructs, a method it calls, an operator,
    /// a constant. Counting those separates `counts` — which names `HashMap`,
    /// `entry`, `or_default`, `+=` and `1` — from a bare transform that names
    /// nothing.
    pub fn anchors(&self) -> u32 {
        let own = match self {
            Self::Construct(_) | Self::Path(_) | Self::Number(_) | Self::Constant(_) => 1,
            Self::Variant { .. } => 1,
            Self::Method { .. } | Self::Field { .. } => 1,
            Self::Assign { operator, .. }
            | Self::Binary { operator, .. }
            | Self::Unary { operator, .. } => u32::from(!operator.is_empty()),
            // Indexing names an operation the way a method call does. A span
            // names only a shape, so like a traversal it anchors nothing of its
            // own and is counted through its endpoints.
            Self::Index { .. } | Self::Swap { .. } => 1,
            Self::Collect { container, .. } => u32::from(container.is_some()),
            // what the arms name is the whole content of a decision
            Self::Select { arms, .. } => arms.iter().map(|arm| arm.pattern.anchors()).sum(),
            _ => 0,
        };
        own + self
            .children()
            .iter()
            .map(|child| child.anchors())
            .sum::<u32>()
    }

    /// How many distinct things this form leaves open.
    ///
    /// A hole matches anything, so it is the opposite of an anchor: it is what
    /// a behavior declines to specify. Counting holes is what makes it possible
    /// to say whether a form describes more than it leaves to chance.
    pub fn holes(&self) -> u32 {
        let mut found = Vec::new();
        self.collect_holes(&mut found);
        u32::try_from(found.len()).unwrap_or(u32::MAX)
    }

    fn collect_holes(&self, found: &mut Vec<u32>) {
        if let Self::Free(index) = self
            && !found.contains(index)
        {
            found.push(*index);
        }
        for child in self.children() {
            child.collect_holes(found);
        }
    }

    /// How deeply this form nests.
    ///
    /// Behavior is a small shape. A form that nests dozens of levels came from
    /// a whole subsystem rather than an operation, will never match anything,
    /// and is deep enough to defeat ordinary readers: `serde_json` refuses more
    /// than 128 levels by default.
    pub fn depth(&self) -> u32 {
        1 + self
            .children()
            .iter()
            .map(|child| child.depth())
            .max()
            .unwrap_or(0)
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

    /// Whether a form describes work rather than plumbing.
    #[must_use]
    pub fn describes_work(&self) -> bool {
        match self {
            Self::Traverse { .. }
            | Self::Sift { .. }
            | Self::Transform { .. }
            | Self::Retain { .. }
            | Self::Accumulate { .. }
            | Self::Pairwise { .. }
            | Self::Repeat { .. }
            | Self::Swap { .. }
            | Self::Collect { .. } => true,
            _ => self.children().into_iter().any(Self::describes_work),
        }
    }

    /// Whether a form chooses among alternatives it names.
    ///
    /// One arm is not a decision — `match x { Some(v) => v }` says only that a
    /// value was unwrapped, which most code does somewhere. Two named
    /// alternatives is the point at which the shape belongs to a particular
    /// type's API.
    fn describes_decision(&self) -> bool {
        if let Self::Select { arms, .. } = self {
            let named = arms
                .iter()
                .filter_map(|arm| match &arm.pattern {
                    Pattern::Variant { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>();
            if named.len() >= 2 {
                return true;
            }
        }
        self.children().into_iter().any(Self::describes_decision)
    }

    /// Whether a form describes something that can be compared across
    /// libraries.
    ///
    /// Derivation used to demand a sequence operation, which confined it to
    /// iterator behaviors and rejected everything else a library does. What
    /// actually has to hold is that the form describes a *decision or a
    /// traversal* rather than plumbing: iterating over something, or choosing
    /// among named alternatives. A getter, a delegation, or a struct literal
    /// describes neither, and would collide with unrelated code wherever it
    /// appeared.
    ///
    /// This is a property of the form and names no language, which is why it
    /// lives here: it was measured to hold unchanged for TypeScript-derived
    /// forms before a second frontend existed to need it.
    #[must_use]
    pub fn is_comparable(&self) -> bool {
        self.describes_work() || self.describes_decision()
    }

    /// Whether a derived behavior is specific enough to report when matched.
    ///
    /// The last condition is what separates a behavior from a shape.
    /// `Option::map_or` is `match self { Some(t) => f(t), None => default }`:
    /// two named alternatives, and everything else a hole. It therefore
    /// describes *every* way of consuming an `Option`, subsumes the narrower
    /// behaviors, and reported nine hundred times across five hundred crates —
    /// technically right and useless. `unwrap_or` says the same thing about
    /// `None` but is concrete about `Some`, and stays.
    ///
    /// So a behavior must name at least as much as it leaves open. This is a
    /// property of the form rather than a threshold chosen to make a number
    /// look good: a form with more holes than anchors matches more situations
    /// than it distinguishes.
    #[must_use]
    pub fn is_reportable(&self) -> bool {
        self.size() >= MINIMUM_REPORTABLE_SIZE
            && self.anchors() >= MINIMUM_ANCHORS
            && self.anchors() >= self.holes()
            && self.is_comparable()
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
            Self::Constant(value) => write!(formatter, "(const {value})"),
            Self::Construct(name) => write!(formatter, "(construct {name})"),
            Self::Variant { name, payload } => {
                write!(formatter, "(variant {name}")?;
                for value in payload {
                    write!(formatter, " {value}")?;
                }
                formatter.write_str(")")
            }
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
                direction,
            } => {
                let name = match direction {
                    Direction::Forward => "traverse",
                    Direction::Backward => "traverse-back",
                };
                write!(formatter, "({name} {sequence} {item} {body})")
            }
            Self::Sift {
                sequence,
                item,
                body,
            } => write!(formatter, "(sift {sequence} {item} {body})"),
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
            Self::Pairwise {
                sequence,
                left,
                right,
                body,
                coverage,
            } => {
                let kind = match coverage {
                    Coverage::Once => "pairwise",
                    Coverage::BothWays => "pairwise-both-ways",
                    Coverage::Adjacent => "pairwise-adjacent",
                };
                write!(formatter, "({kind} {sequence} {left} {right} {body})")
            }
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
            Self::Unary { operator, value } => write!(formatter, "(unary {operator} {value})"),
            Self::Repeat { condition, body } => write!(formatter, "(repeat {condition} {body})"),
            Self::Swap {
                sequence,
                left,
                right,
            } => write!(formatter, "(swap {sequence} {left} {right})"),
            Self::Index { sequence, position } => {
                write!(formatter, "(index {sequence} {position})")
            }
            Self::Span {
                start,
                end,
                inclusive,
            } => {
                let kind = if *inclusive { "span=" } else { "span" };
                write!(formatter, "({kind} {start} {end})")
            }
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
            Self::Select { scrutinee, arms } => {
                write!(formatter, "(select {scrutinee}")?;
                for arm in arms {
                    write!(formatter, " {} => {}", arm.pattern, arm.body)?;
                }
                formatter.write_str(")")
            }
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

/// Assigns normalized roles to names as a language normalizer walks a body.
///
/// A name bound by the code becomes a local, numbered by binding order.
/// Anything else is free, numbered by first appearance.
#[derive(Debug, Default)]
pub struct Roles {
    roles: Vec<(String, Form)>,
    /// Names that hold values supplied from outside the body.
    values: Vec<String>,
    /// Every role assigned, with the identifier it was assigned to.
    ///
    /// Nothing in matching may consult this: which identifier an author chose
    /// is not behavior, and a form that carried it would stop comparing. It is
    /// kept beside the form, not in it, so that a consumer which needs the
    /// names — anything emitting code rather than comparing it — can ask.
    /// Shadowing appends rather than replaces, because a role that was
    /// displaced still names what it named.
    ledger: Vec<(Form, String)>,
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
        self.ledger.push((form.clone(), name.to_owned()));
        form
    }

    /// What each role was called in the source it was lifted from.
    pub fn ledger(&self) -> &[(Form, String)] {
        &self.ledger
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
        self.ledger.push((form.clone(), name.to_owned()));
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
            direction: Direction::Forward,
        }
    }

    fn pairwise(coverage: Coverage) -> Form {
        Form::Pairwise {
            sequence: Box::new(Form::Free(0)),
            left: Box::new(Pattern::Binding(0)),
            right: Box::new(Pattern::Binding(1)),
            body: Box::new(Form::Binary {
                operator: "==".to_owned(),
                left: Box::new(Form::Local(0)),
                right: Box::new(Form::Local(1)),
            }),
            coverage,
        }
    }

    /// A walk over pairs takes part in matching like every other form.
    ///
    /// Adding a variant without teaching the unifier about it does not fail to
    /// compile: the fallthrough answers `false`, so the form silently matches
    /// nothing and every behavior written over it goes quiet.
    #[test]
    fn a_walk_over_pairs_matches_itself() {
        assert!(pairwise(Coverage::Once).contains(&pairwise(Coverage::Once)));
        assert!(
            Form::Sequence(vec![Form::Literal, pairwise(Coverage::Once)])
                .contains(&pairwise(Coverage::Once))
        );
    }

    /// Seeing each pair once is not seeing it both ways round.
    ///
    /// The two reach the same pairs and differ in how often, which is behavior
    /// for anything that counts.
    #[test]
    fn the_two_coverages_do_not_match_each_other() {
        assert!(!pairwise(Coverage::Once).contains(&pairwise(Coverage::BothWays)));
        assert!(!pairwise(Coverage::BothWays).contains(&pairwise(Coverage::Once)));
    }

    /// Code that does a thing four times has four findings.
    ///
    /// Reporting only the first meant a reader who fixed what they were shown
    /// had no way to learn the rest existed — they surfaced one re-run at a
    /// time. On clippy's `manual_unwrap_or` corpus this alone was the
    /// difference between 7 of 20 and 19 of 20, because those tests write the
    /// same `match` out repeatedly inside one function.
    #[test]
    fn every_occurrence_is_located_not_only_the_first() {
        let body = Form::Sequence(vec![traverse(), Form::Literal, traverse()]);
        assert_eq!(
            body.locate_all(&traverse()),
            vec![0..1, 2..3],
            "both traversals are the behavior, and the statement between them is not"
        );
        assert_eq!(
            body.locate(&traverse()),
            Some(0..1),
            "locate stays first-only"
        );
    }

    /// Matches do not overlap, or a behavior spread over several statements
    /// would be reported once per statement it touches.
    #[test]
    fn located_occurrences_do_not_overlap() {
        let pair = Form::Sequence(vec![traverse(), traverse()]);
        let pattern = Form::Sequence(vec![traverse(), traverse()]);
        assert_eq!(pair.locate_all(&pattern), vec![0..2]);
    }

    /// A forward walk must serialize exactly as it did before it had a
    /// direction, or every published behavior that traverses anything changes
    /// digest without changing meaning.
    #[test]
    fn adding_a_direction_did_not_change_what_a_forward_walk_serializes_to() {
        let written_before_the_field_existed =
            r#"{"traverse":{"sequence":{"free":0},"item":{"binding":1},"body":{"local":1}}}"#;
        let forward = Form::Traverse {
            sequence: Box::new(Form::Free(0)),
            item: Box::new(Pattern::Binding(1)),
            body: Box::new(Form::Local(1)),
            direction: Direction::Forward,
        };
        assert_eq!(
            serde_json::to_string(&forward).unwrap(),
            written_before_the_field_existed,
            "a forward walk is written exactly as it always was"
        );
        let decoded: Form = serde_json::from_str(written_before_the_field_existed).unwrap();
        assert_eq!(
            decoded, forward,
            "a pack written before the field still reads"
        );

        let backward = Form::Traverse {
            sequence: Box::new(Form::Free(0)),
            item: Box::new(Pattern::Binding(1)),
            body: Box::new(Form::Local(1)),
            direction: Direction::Backward,
        };
        assert!(
            serde_json::to_string(&backward)
                .unwrap()
                .contains("backward"),
            "a backwards walk is recorded, because it is news"
        );
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
