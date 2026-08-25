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

mod matching;
mod renaming;
mod simplify;

use matching::Bindings;
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
    /// Only the distinct-pairs walk reduces to this. Walking adjacent pairs is
    /// `windows(2)` and a different coverage; walking every ordered pair
    /// including an element with itself is a third. Both stay two traversals
    /// until something needs them, because a coverage field with one inhabitant
    /// says nothing and a wrong one would claim a walk the code does not make.
    Pairwise {
        sequence: Box<Form>,
        left: Box<Pattern>,
        right: Box<Pattern>,
        body: Box<Form>,
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
            } => Self::Pairwise {
                sequence: apply(sequence),
                left: left.clone(),
                right: right.clone(),
                body: apply(body),
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
            Self::Index { .. } => 1,
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

    /// Whether this form contains `pattern` anywhere within it.
    ///
    /// Repository code rarely consists of nothing but the behavior in question,
    /// so a match is a subtree relationship rather than whole-body equality.
    pub fn contains(&self, pattern: &Self) -> bool {
        self.search(pattern, false)
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
        if self.matches_with(pattern, false) || self.contains_steps_with(pattern, false) {
            found.push(self);
        }
        for child in self.children() {
            child.find(pattern, found);
        }
    }

    /// Where in a sequence a pattern matches, as a range of steps.
    ///
    /// A finding is only useful if it points at the code. The steps that make
    /// up a behavior need not be adjacent, so this reports from the first to
    /// the last of them: the extent the behavior is spread over, which is
    /// exactly what a reader has to look at.
    pub fn matching_steps(&self, pattern: &Self) -> Option<std::ops::Range<usize>> {
        let Self::Sequence(haystack) = self else {
            return None;
        };
        let steps = match pattern {
            Self::Sequence(steps) => steps.as_slice(),
            other => std::slice::from_ref(other),
        };
        let (first, rest) = steps.split_first()?;
        if steps.len() > haystack.len() {
            return None;
        }
        haystack.iter().enumerate().find_map(|(start, candidate)| {
            let mut bindings = Bindings::default();
            if !bindings.form(candidate, first) {
                return None;
            }
            let mut matched = vec![start];
            bindings
                .follow_recording(&haystack[start + 1..], rest, start + 1, &mut matched)
                .then(|| start..matched.last().map_or(start, |last| last + 1))
        })
    }

    /// Where a pattern matches, trying the behavior without its closing
    /// reference when the whole of it does not appear.
    pub fn locate(&self, pattern: &Self) -> Option<std::ops::Range<usize>> {
        self.matching_steps(pattern).or_else(|| {
            pattern
                .without_result_reference()
                .and_then(|work| self.matching_steps(&work))
        })
    }

    /// Every place a pattern matches, not just the first.
    ///
    /// A function that writes the same `match` out four times has done the
    /// thing four times, and reporting one of them surfaces the rest a re-run
    /// at a time. Matches are non-overlapping: a run of steps that has been
    /// claimed cannot also be the start of the next match, or one behavior
    /// spread over several statements would be reported once per statement it
    /// touches.
    pub fn locate_all(&self, pattern: &Self) -> Vec<std::ops::Range<usize>> {
        let Self::Sequence(haystack) = self else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut from = 0;
        while from < haystack.len() {
            let rest = Self::Sequence(haystack[from..].to_vec());
            let Some(range) = rest.locate(pattern) else {
                break;
            };
            found.push(from + range.start..from + range.end);
            from += range.end.max(range.start + 1);
        }
        found
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

    /// Whether this form performs `pattern` among other work.
    ///
    /// Real loops are rarely dedicated to one thing. A loop that groups values
    /// and also counts them still groups them, and saying so is useful even
    /// though the replacement is not mechanical. A loop that `break`s or
    /// `continue`s is different in kind: it does not visit every element, so it
    /// is not the behavior at all, however much of the shape it shares.
    pub fn contains_fused(&self, pattern: &Self) -> bool {
        self.search(pattern, true)
    }

    /// Look for a pattern anywhere in this form, by every route that counts as
    /// finding it: as a whole, as a run of consecutive steps, and without the
    /// closing reference a library function needs but inline code does not.
    fn search(&self, pattern: &Self, fused: bool) -> bool {
        if self.matches_with(pattern, fused) || self.contains_steps_with(pattern, fused) {
            return true;
        }
        if let Some(work) = pattern.without_result_reference()
            && (self.matches_with(&work, fused) || self.contains_steps_with(&work, fused))
        {
            return true;
        }
        self.children()
            .into_iter()
            .any(|child| child.search(pattern, fused))
    }

    fn matches_with(&self, pattern: &Self, fused: bool) -> bool {
        let mut bindings = Bindings::with_fusion(fused);
        bindings.form(self, pattern)
    }

    /// Whether a sequence performs the pattern's steps, in order, ignoring
    /// unrelated statements written among them.
    ///
    /// Code does not lay a behavior out contiguously. An accumulator is
    /// declared, then something unrelated, then the loop that fills it. Those
    /// interruptions are only unrelated if they leave the behavior alone, so a
    /// statement may be stepped over exactly when it touches nothing the match
    /// has bound. A statement that reads or rewrites the accumulator is part of
    /// what the code does and cannot be skipped past.
    fn contains_steps_with(&self, pattern: &Self, fused: bool) -> bool {
        let (Self::Sequence(haystack), Self::Sequence(steps)) = (self, pattern) else {
            return false;
        };
        let Some((first, rest)) = steps.split_first() else {
            return false;
        };
        if steps.len() > haystack.len() {
            return false;
        }
        haystack.iter().enumerate().any(|(start, candidate)| {
            let mut bindings = Bindings::with_fusion(fused);
            bindings.form(candidate, first) && bindings.follow(&haystack[start + 1..], rest)
        })
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

    /// The one sequence a body indexes at every named position.
    ///
    /// A loop bound is often a variable rather than the sequence's own length —
    /// `for i in 0..n` far more often than `for i in 0..v.len()` — so the span
    /// does not always say what is being walked. The body does: whatever it
    /// reads at those positions is the sequence, and it has to be exactly one
    /// of them, or the loop is walking positions into two things at once and is
    /// not a walk over either.
    fn sole_indexed_sequence(&self, positions: &[u32]) -> Option<&Self> {
        let mut sequence = None;
        let mut seen = Vec::new();
        self.collect_indexed(positions, &mut sequence, &mut seen)?;
        positions
            .iter()
            .all(|position| seen.contains(position))
            .then_some(sequence)?
    }

    /// Gather the sequence indexed at each position, failing on disagreement.
    fn collect_indexed<'a>(
        &'a self,
        positions: &[u32],
        sequence: &mut Option<&'a Self>,
        seen: &mut Vec<u32>,
    ) -> Option<()> {
        if let Self::Index {
            sequence: indexed,
            position,
        } = self
            && let Self::Local(index) = position.as_ref()
            && positions.contains(index)
        {
            if sequence.is_some_and(|found| found != indexed.as_ref()) {
                return None;
            }
            *sequence = Some(indexed.as_ref());
            if !seen.contains(index) {
                seen.push(*index);
            }
        }
        for child in self.children() {
            child.collect_indexed(positions, sequence, seen)?;
        }
        Some(())
    }

    /// Whether a body reads a sequence only by indexing it at named positions.
    ///
    /// This is the licence to forget the index. `for i in 0..v.len()` visits
    /// each element of `v` only when `i` is used for nothing but `v[i]` and `v`
    /// is reached no other way: `v[i + 1]` looks at a different element,
    /// `w[i]` at a different sequence, and `v.swap(i, j)` at the sequence
    /// itself. Each of those makes the loop something other than an element
    /// visit, and each fails this test.
    fn indexed_only(&self, sequence: &Self, positions: &[u32]) -> bool {
        if let Self::Index {
            sequence: indexed,
            position,
        } = self
            && indexed.as_ref() == sequence
            && let Self::Local(index) = position.as_ref()
            && positions.contains(index)
        {
            return true;
        }
        if self == sequence {
            return false;
        }
        if let Self::Local(index) = self
            && positions.contains(index)
        {
            return false;
        }
        self.children()
            .into_iter()
            .all(|child| child.indexed_only(sequence, positions))
    }

    /// Whether anything here writes through an index into a sequence.
    ///
    /// `v[i] = x` passes `indexed_only` and is not an element visit: it
    /// replaces the element rather than looking at it, and forgetting the index
    /// would turn a write into a read of the value written.
    fn writes_indexed(&self, sequence: &Self) -> bool {
        if let Self::Assign { target, .. } = self
            && target.indexes(sequence)
        {
            return true;
        }
        self.children()
            .into_iter()
            .any(|child| child.writes_indexed(sequence))
    }

    /// Whether this form indexes into a sequence anywhere.
    fn indexes(&self, sequence: &Self) -> bool {
        if let Self::Index {
            sequence: indexed, ..
        } = self
            && indexed.as_ref() == sequence
        {
            return true;
        }
        self.children()
            .into_iter()
            .any(|child| child.indexes(sequence))
    }

    /// The same body with each licensed indexing replaced by the element.
    ///
    /// Only positions `indexed_only` has already accepted are rewritten, so the
    /// name that held an index comes to hold what it indexed.
    fn with_indexed_elements(&self, sequence: &Self, positions: &[u32]) -> Self {
        if let Self::Index {
            sequence: indexed,
            position,
        } = self
            && indexed.as_ref() == sequence
            && let Self::Local(index) = position.as_ref()
            && positions.contains(index)
        {
            return Self::Local(*index);
        }
        self.map_children(&|child| child.with_indexed_elements(sequence, positions))
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
            } => write!(formatter, "(pairwise {sequence} {left} {right} {body})"),
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
