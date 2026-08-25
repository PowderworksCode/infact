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

use std::cell::Cell;

use crate::{Direction, Form, Pattern};

/// How many times to sweep before giving up.
///
/// The laws shrink a form or leave it alone, so a fixpoint is normally reached
/// in a few passes. The bound exists because unfolding can in principle keep
/// finding work, and a normalizer that fails to terminate is worse than one
/// that stops early.
const MAX_SWEEPS: usize = 8;

/// How many calls one `simplify` may replace with the body they name.
///
/// A bound is necessary because unfolding can be made to run forever by code
/// that applies a function to itself — the Y combinator is the shortest way,
/// and CPython's `test_inspect` writes one. Nothing about the binding group
/// says so in advance, so this is what stops it.
///
/// IT IS ALSO A TUNING KNOB, which the first version of this comment claimed it
/// was not. Measured over CPython's `Lib`, the total size of every form the
/// Python frontend produces still rises with every increase — 2,390,857 nodes
/// at 4, 2,418,798 at 16, 2,443,675 at 64, with no plateau anywhere. Real
/// bodies consume all of it, because unfolding a call into a body that is used
/// twice makes the form bigger on purpose: that is how a caller's inlined shape
/// comes to match a library's.
///
/// So the value trades how much a form says against how large it gets, and it
/// has NOT been calibrated against whether matching improves — that needs a
/// matching corpus, which Python does not have yet. 64 is the value the
/// pathological cases were fixed at, not a measured optimum.
const MAX_UNFOLDS: u32 = 64;

impl Form {
    /// Rewrite until no law applies.
    pub fn simplify(&self) -> Self {
        // The fuel spans the whole call rather than one pass. Per-pass, eight
        // sweeps compound: each one unfolds a self-application 64 levels deeper
        // than the last, and the form a bounded rewrite left behind was still
        // large enough to exhaust a debug build's stack when anything walked it.
        let fuel = Cell::new(MAX_UNFOLDS);
        let mut current = self.clone();
        for _ in 0..MAX_SWEEPS {
            let next = current.sweep(&fuel);
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
            Self::Pairwise { left, right, .. } => vec![left, right],
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
    fn sweep(&self, fuel: &Cell<u32>) -> Self {
        let rebuilt = self.map_children(&|child| child.sweep(fuel));
        rebuilt
            .as_fused()
            .or_else(|| rebuilt.as_searched())
            .or_else(|| rebuilt.as_returned_sequence())
            .or_else(|| rebuilt.as_optional_search())
            .or_else(|| rebuilt.as_escape())
            .or_else(|| rebuilt.as_traversal())
            .or_else(|| rebuilt.as_canonical_arithmetic())
            .or_else(|| rebuilt.as_element_traversal())
            .or_else(|| rebuilt.as_pairwise())
            .or_else(|| rebuilt.as_recovered_escape())
            .or_else(|| rebuilt.as_unfolded(fuel))
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

    /// A walk that escapes with a value and otherwise yields nothing is a
    /// search for an optional.
    ///
    /// Rust says this in its types: `find` returns `Option<T>`, so the form
    /// already carries `Some` around what it escaped with. JavaScript says it
    /// in a convention — return the element, or fall off the end and yield
    /// `undefined` — and the type is `T | undefined`, which is the same claim
    /// spelled without a constructor. Making the constructor explicit is what
    /// lets one behavior derived from either language match code written in the
    /// other, and it is why `find` and `some` do not collapse: `some` yields a
    /// literal, not an absence.
    ///
    /// Only a walk whose escapes are all bare is wrapped, so a form that
    /// already speaks in optionals is left exactly as it is.
    fn as_optional_search(&self) -> Option<Self> {
        let Self::Sequence(steps) = self else {
            return None;
        };
        let [
            Self::Traverse {
                sequence,
                item,
                body,
                direction,
            },
            tail,
        ] = steps.as_slice()
        else {
            return None;
        };
        if !matches!(tail, Self::Variant { name, payload } if name == "None" && payload.is_empty())
        {
            return None;
        }
        let mut escapes = Vec::new();
        escape_values(body, &mut escapes);
        // nothing escapes: this walks for effect and yields nothing, which is a
        // traversal rather than a search
        if escapes.is_empty() {
            return None;
        }
        if escapes
            .iter()
            .any(|value| matches!(value, Self::Variant { name, .. } if name == "Some"))
        {
            return None;
        }
        Some(Self::Sequence(vec![
            Self::Traverse {
                sequence: sequence.clone(),
                item: item.clone(),
                body: Box::new(body.escapes_wrapped()),
                direction: *direction,
            },
            tail.clone(),
        ]))
    }

    /// Returning what a sequence of steps produces is doing them and returning.
    ///
    /// A block's value is its last step, so `return (do A B)` performs `A` and
    /// returns `B`, which is what `do A (return B)` says. The two arise from
    /// writing one computation as an expression or as statements — a library
    /// implementing a search writes the loop, and a caller writes
    /// `return xs.filter(p)[0]` — and they have to meet.
    fn as_returned_sequence(&self) -> Option<Self> {
        let Self::Return(value) = self else {
            return None;
        };
        let Self::Sequence(steps) = value.as_ref() else {
            return None;
        };
        let (last, rest) = steps.split_last()?;
        // a block with one step is that step, and moving a return into it would
        // only shuffle the same form back and forth
        if rest.is_empty() {
            return None;
        }
        let mut moved = rest.to_vec();
        moved.push(Self::Return(Box::new(last.clone())));
        Some(Self::Sequence(moved))
    }

    /// The first of what was retained is a search.
    ///
    /// Filtering a sequence and taking the first survivor visits every element
    /// only because that is how it is written; what it describes is stopping at
    /// the first one that satisfies the test. That is the same computation a
    /// loop with an early return performs, and it is what a library offers as a
    /// single operation.
    ///
    /// Taking the *last* survivor is the same search run the other way, and it
    /// reduces to the same shape walking backwards. The direction lives inside
    /// the traversal precisely so that these two do not collapse: expressed as
    /// a reversal wrapped around the sequence it would sit where the derived
    /// behavior has a hole, and a hole absorbs it, so `find` would match
    /// `findLast` code and name the wrong API.
    fn as_searched(&self) -> Option<Self> {
        let Self::Method {
            name,
            receiver,
            arguments,
        } = self
        else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let direction = match name.as_str() {
            "first" => Direction::Forward,
            "last" => Direction::Backward,
            _ => return None,
        };
        let Self::Retain {
            sequence,
            item,
            body,
        } = receiver.as_ref()
        else {
            return None;
        };
        let Pattern::Binding(index) = item.as_ref() else {
            return None;
        };
        Some(Self::Sequence(vec![
            Self::Traverse {
                sequence: sequence.clone(),
                item: item.clone(),
                body: Box::new(Self::Branch {
                    condition: body.clone(),
                    consequence: Box::new(Self::Return(Box::new(Self::Local(*index)))),
                    alternative: None,
                }),
                direction,
            },
            Self::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            },
        ]))
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

    /// An index loop that only ever indexes is a walk over the elements.
    ///
    /// `for i in 0..v.len() { total += v[i] }` and `for x in v { total += x }`
    /// are the same visit written two ways, and only the second reduced to a
    /// traversal of `v`. The first traversed a span of integers, which is a
    /// traversal of something else entirely and compares against nothing.
    ///
    /// The condition is narrow on purpose: `i` must be used for nothing but
    /// `v[i]`, `v` must be reached no other way, and nothing may write through
    /// the index. Anything else — `v[i + 1]`, `v.swap(i, j)`, `v[i] = x` — is
    /// not an element visit, and forgetting the index there would report a walk
    /// the code does not make.
    fn as_element_traversal(&self) -> Option<Self> {
        let Self::Traverse {
            sequence,
            item,
            body,
            direction,
        } = self
        else {
            return None;
        };
        let Pattern::Binding(index) = item.as_ref() else {
            return None;
        };
        let source = whole_index_span(sequence)?;
        if !body.indexed_only(source, &[*index]) || body.writes_indexed(source) {
            return None;
        }
        Some(Self::Traverse {
            sequence: Box::new(source.clone()),
            item: item.clone(),
            body: Box::new(body.with_indexed_elements(source, &[*index])),
            direction: *direction,
        })
    }

    /// Two nested loops that reach each pair of one sequence once.
    ///
    /// Both spellings reduce here: index arithmetic over the upper or lower
    /// triangle, and `enumerate` with a `skip` past the outer position. What
    /// they have in common is which pairs get visited, and that is the only
    /// thing `Pairwise` records.
    ///
    /// A nested loop whose inner bound is the whole sequence is NOT this, even
    /// with an `i != j` guard inside: the guard is written over the indices,
    /// and this rewrite forgets them. Recognizing that spelling means keeping
    /// the indices as well as the elements, so it stays two traversals.
    fn as_pairwise(&self) -> Option<Self> {
        let Self::Traverse {
            sequence: outer,
            item: first,
            body,
            direction: Direction::Forward,
        } = self
        else {
            return None;
        };
        let inner_traversal = match body.as_ref() {
            Self::Sequence(steps) => match steps.as_slice() {
                [only] => only,
                _ => return None,
            },
            other => other,
        };
        let Self::Traverse {
            sequence: inner,
            item: second,
            body: inner_body,
            direction: Direction::Forward,
        } = inner_traversal
        else {
            return None;
        };
        self.as_indexed_pairwise(outer, first, inner, second, inner_body)
            .or_else(|| Self::as_enumerated_pairwise(outer, first, inner, second, inner_body))
    }

    /// The index spelling: two spans over one sequence's positions.
    fn as_indexed_pairwise(
        &self,
        outer: &Self,
        first: &Pattern,
        inner: &Self,
        second: &Pattern,
        body: &Self,
    ) -> Option<Self> {
        let source = whole_index_span(outer)?;
        let (Pattern::Binding(left), Pattern::Binding(right)) = (first, second) else {
            return None;
        };
        if !covers_each_pair_once(inner, *left, source) {
            return None;
        }
        let positions = [*left, *right];
        if !body.indexed_only(source, &positions) || body.writes_indexed(source) {
            return None;
        }
        Some(Self::Pairwise {
            sequence: Box::new(source.clone()),
            left: Box::new(first.clone()),
            right: Box::new(second.clone()),
            body: Box::new(body.with_indexed_elements(source, &positions)),
        })
    }

    /// The iterator spelling: `enumerate` outside, `skip(i + 1)` inside.
    ///
    /// The position `enumerate` binds exists only to say where the inner walk
    /// starts. A body that reads it is doing something with the index besides
    /// pairing, so this declines rather than dropping it.
    fn as_enumerated_pairwise(
        outer: &Self,
        first: &Pattern,
        inner: &Self,
        second: &Pattern,
        body: &Self,
    ) -> Option<Self> {
        let source = receiver_of(outer, "enumerate", 0)?;
        let Pattern::Tuple(parts) = first else {
            return None;
        };
        let [Pattern::Binding(position), element] = parts.as_slice() else {
            return None;
        };
        let skipped = receiver_of(inner, "skip", 1)?;
        if skipped != source || !skips_past(inner, *position) || body.references_local(*position) {
            return None;
        }
        Some(Self::Pairwise {
            sequence: Box::new(source.clone()),
            left: Box::new(element.clone()),
            right: Box::new(second.clone()),
            body: Box::new(body.clone()),
        })
    }

    /// Arithmetic that means one thing written several ways.
    ///
    /// `len - 1 - i` and `len - i - 1` are the same bound and different trees,
    /// and a loop bound is exactly where a hand-rolled algorithm keeps its
    /// content: recognizing which pairs a nested loop visits is reading its
    /// endpoints. Three rewrites travel together because they only reach a
    /// normal form together — a subtraction chain re-associates onto one
    /// subtrahend, that subtrahend's operands then take a deterministic order,
    /// and constant operands fold.
    ///
    /// Reordering assumes the operands can be evaluated in either order. For
    /// the integer index arithmetic this exists to canonicalize that always
    /// holds; for an operand that calls something with an effect it is a
    /// generalization, and the same one `Select` already makes by sorting arms.
    fn as_canonical_arithmetic(&self) -> Option<Self> {
        let Self::Binary {
            operator,
            left,
            right,
        } = self
        else {
            return None;
        };
        // `(a - b) - c` is `a - (b + c)`, which is what lets the two spellings
        // of a descending loop bound meet.
        if operator == "-"
            && let Self::Binary {
                operator: inner,
                left: first,
                right: second,
            } = left.as_ref()
            && inner == "-"
        {
            return Some(Self::Binary {
                operator: "-".to_owned(),
                left: first.clone(),
                right: Box::new(Self::Binary {
                    operator: "+".to_owned(),
                    left: second.clone(),
                    right: right.clone(),
                }),
            });
        }
        if let (Self::Number(first), Self::Number(second)) = (left.as_ref(), right.as_ref())
            && let Some(folded) = fold(operator, first, second)
        {
            return Some(Self::Number(folded));
        }
        // A commutative operator says the same thing either way round, so the
        // two orders must not be two forms. The order is the form's own, which
        // is arbitrary but total, and swapping only ever moves toward it, so
        // this cannot swap back and forth forever.
        if COMMUTATIVE.contains(&operator.as_str()) && right < left {
            return Some(Self::Binary {
                operator: operator.clone(),
                left: right.clone(),
                right: left.clone(),
            });
        }
        None
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
            direction: Direction::Forward,
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
    fn as_unfolded(&self, fuel: &Cell<u32>) -> Option<Self> {
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
        let bindings = unfoldable(&bindings);
        if bindings.is_empty() {
            return None;
        }
        let rewritten = steps
            .iter()
            .map(|step| step.apply_bindings(&bindings, fuel))
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
    /// `fuel` bounds how many calls one pass may replace.
    ///
    /// [`unfoldable`] refuses the bindings whose *names* cycle, and that is not
    /// enough on its own: a recursion can arrive through an ARGUMENT instead.
    /// The Y combinator is the shortest example and CPython's `test_inspect`
    /// contains it —
    ///
    /// ```text
    ///   def Y(le):
    ///       def g(f):
    ///           return le(lambda x: f(f)(x))
    ///       return g(g)
    /// ```
    ///
    /// — where `g` names nothing recursive, and substituting `g` for `f`
    /// produces `f(f)` again. No analysis of the binding group can see that
    /// coming, so unfolding is bounded rather than made clever. Running out
    /// leaves a partly unfolded form, which is the same thing `MAX_SWEEPS`
    /// already does one level up.
    fn apply_bindings(&self, bindings: &[(u32, Self)], fuel: &Cell<u32>) -> Self {
        if let Self::Call { callee, arguments } = self
            && let Self::Local(index) = callee.as_ref()
            && let Some((_, Self::Lambda { parameters, body })) =
                bindings.iter().find(|(bound, _)| bound == index)
            && fuel.get() > 0
        {
            fuel.set(fuel.get() - 1);
            let mut substituted = body.as_ref().clone();
            for (parameter, argument) in parameters.iter().zip(arguments) {
                if let Pattern::Binding(bound) = parameter {
                    substituted = substituted.substitute(*bound, argument);
                }
            }
            return substituted.apply_bindings(bindings, fuel);
        }
        self.map_children(&|child| child.apply_bindings(bindings, fuel))
    }

    /// Replace a bound name with a value throughout.
    fn substitute(&self, index: u32, value: &Self) -> Self {
        match self {
            Self::Local(bound) if *bound == index => value.clone(),
            other => other.map_children(&|child| child.substitute(index, value)),
        }
    }
}

/// The bindings that can be unfolded without the unfolding going on forever.
///
/// Replacing a call with the body it calls does not terminate when the body
/// calls back: a self-recursive local function expands into a copy of itself
/// containing another call to itself, and so on until the stack is gone. A
/// group of local functions that call each other does the same thing one step
/// further out.
///
/// This is not hypothetical and it is not a Python problem, though Python is
/// where it surfaced: `json/encoder.py` defines `_iterencode`,
/// `_iterencode_list` and `_iterencode_dict` inside one factory and has each
/// call the others, and simplifying it aborted the process. Any language whose
/// frontend binds a nested function to a name reaches the same shape.
///
/// A binding is safe when every group member its body names is itself safe,
/// which is a least fixed point over an acyclic subgraph. A binding that names
/// itself is never safe, because it is never in the set being tested against.
/// A binding that only calls safe ones still unfolds, because substituting into
/// a directed acyclic graph terminates.
fn unfoldable(bindings: &[(u32, Form)]) -> Vec<(u32, Form)> {
    let mut safe: Vec<(u32, Form)> = Vec::new();
    loop {
        let next = bindings.iter().find(|(index, lambda)| {
            !safe.iter().any(|(known, _)| known == index)
                && bindings.iter().all(|(other, _)| {
                    !lambda.references_local(*other) || safe.iter().any(|(known, _)| known == other)
                })
        });
        match next {
            Some(binding) => safe.push(binding.clone()),
            None => return safe,
        }
    }
}

/// The sequence whose whole index range a span walks.
///
/// `0..v.len()` reaches every position of `v` and nothing else. An inclusive
/// bound reaches one position past the end, which is a different walk and in
/// Rust a panicking one, so it is not this.
fn whole_index_span(form: &Form) -> Option<&Form> {
    let Form::Span {
        start,
        end,
        inclusive,
    } = form
    else {
        return None;
    };
    if *inclusive || **start != Form::Number("0".to_owned()) {
        return None;
    }
    let Form::Method {
        name,
        receiver,
        arguments,
    } = end.as_ref()
    else {
        return None;
    };
    (name == "len" && arguments.is_empty()).then(|| receiver.as_ref())
}

/// Whether an inner span visits each pair of a sequence exactly once.
///
/// Two spans do it, and they are the two triangles of the index square:
/// `i + 1 .. len` takes every position after the outer one, and `0 .. i` every
/// position before it. Either way each unordered pair is reached once. A table
/// rather than arithmetic — a bound this cannot read is a bound this refuses.
fn covers_each_pair_once(span: &Form, outer: u32, sequence: &Form) -> bool {
    let Form::Span {
        start,
        end,
        inclusive: false,
    } = span
    else {
        return false;
    };
    let above = is_successor_of(start, outer)
        && matches!(whole_index_span(&Form::Span {
            start: Box::new(Form::Number("0".to_owned())),
            end: end.clone(),
            inclusive: false,
        }), Some(walked) if walked == sequence);
    let below = **start == Form::Number("0".to_owned()) && **end == Form::Local(outer);
    above || below
}

/// Whether a form is one more than a named position.
///
/// Canonicalized arithmetic puts the name first, but this accepts either order
/// so that the law does not depend on which way the ordering happened to fall.
fn is_successor_of(form: &Form, local: u32) -> bool {
    let Form::Binary {
        operator,
        left,
        right,
    } = form
    else {
        return false;
    };
    operator == "+"
        && ((**left == Form::Local(local) && **right == Form::Number("1".to_owned()))
            || (**right == Form::Local(local) && **left == Form::Number("1".to_owned())))
}

/// The receiver of a method call with a given name and argument count.
fn receiver_of<'a>(form: &'a Form, method: &str, arity: usize) -> Option<&'a Form> {
    let Form::Method {
        name,
        receiver,
        arguments,
    } = form
    else {
        return None;
    };
    (name == method && arguments.len() == arity).then(|| receiver.as_ref())
}

/// Whether a `skip` starts one past a named position.
fn skips_past(form: &Form, local: u32) -> bool {
    matches!(form, Form::Method { arguments, .. }
        if matches!(arguments.as_slice(), [amount] if is_successor_of(amount, local)))
}

/// Operators whose operands may be written either way round.
///
/// Comparison for equality is here and ordering comparison is not: `a < b` and
/// `b < a` are opposite questions.
const COMMUTATIVE: &[&str] = &["+", "*", "==", "!=", "&", "|", "^", "&&", "||"];

/// Two integer literals combined, when the operator has an integer answer.
///
/// Only integers fold. A float literal is held as written because its value
/// does not survive the round trip, and division is left alone because it
/// truncates in some languages and not others.
fn fold(operator: &str, left: &str, right: &str) -> Option<String> {
    let (left, right) = (left.parse::<i128>().ok()?, right.parse::<i128>().ok()?);
    let value = match operator {
        "+" => left.checked_add(right),
        "-" => left.checked_sub(right),
        "*" => left.checked_mul(right),
        _ => None,
    }?;
    Some(value.to_string())
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

/// Every value a form escapes with, however deeply nested.
fn escape_values<'a>(form: &'a Form, into: &mut Vec<&'a Form>) {
    if let Form::Return(value) = form {
        into.push(value.as_ref());
    }
    for child in form.children() {
        escape_values(child, into);
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

impl Form {
    /// The lazy operation a terminal search stands for, if it is one.
    ///
    /// THIS IS NOT A LAW, and it is deliberately not part of `simplify`. The
    /// two forms are not equal: a search stops at the first hit and a lazy
    /// adaptor does not. It holds only where the caller already knows it is
    /// looking at one step of a lazy adaptor — a callable that merely
    /// constructs a type, followed into that type's `next`. `FilterMap::next`
    /// delegates to `find_map`, so what a naive derivation stores under
    /// `filter_map` IS `find_map`, and the two are indistinguishable until this
    /// lift separates them.
    ///
    /// One step of the adaptor is the whole operation with the stop removed, so
    /// the rewrite is to drop the stop:
    ///
    /// ```text
    /// (do (traverse s x (select SCRUT (None) => (lit) (Some p) => (return (Some p)))) (None))
    ///   -> (sift s x SCRUT)
    /// (do (traverse s x (branch COND (return (Some x)))) (None))
    ///   -> (retain s x COND)
    /// ```
    ///
    /// Returns `None` when the form is not that shape, and when it is that
    /// shape but the adaptor carries state — see [`Form::is_stateful_step`].
    pub fn lifted_from_one_step(&self) -> Option<Self> {
        let Self::Sequence(parts) = self else {
            return None;
        };
        let [
            Self::Traverse {
                sequence,
                item,
                body,
                direction,
            },
            exhausted,
        ] = parts.as_slice()
        else {
            return None;
        };
        if !is_none_variant(exhausted) {
            return None;
        }
        // A lazy adaptor walks its sequence forwards. Lifting a backwards
        // search into one would describe an operation nobody wrote, so the
        // backwards case declines rather than being quietly turned around.
        if !direction.is_forward() {
            return None;
        }
        // A stateful adaptor's `next` is not one step of anything expressible
        // here, and admitting it would be undetectable downstream.
        if self.is_stateful_step() {
            return None;
        }
        let carried = one_step_yield(body)?;
        Some(match carried {
            OneStep::Sifted(scrutinee) => Self::Sift {
                sequence: sequence.clone(),
                item: item.clone(),
                body: Box::new(scrutinee),
            },
            OneStep::Retained(condition) => Self::Retain {
                sequence: sequence.clone(),
                item: item.clone(),
                body: Box::new(condition),
            },
        })
    }

    /// Whether a `next` carries state across calls, rather than reading one
    /// element and answering.
    ///
    /// Fails closed: `Flatten`, `Peekable` and `Chunks` must refuse. They do not
    /// derive today, so refusing costs nothing, and admitting them would put a
    /// one-step reading on something that has no one-step reading.
    fn is_stateful_step(&self) -> bool {
        self.assigns_to_a_field() || self.sequences_traversed() > 1
    }

    /// Whether anything here writes through a field, which is how an adaptor
    /// remembers where it was.
    fn assigns_to_a_field(&self) -> bool {
        if let Self::Assign { target, .. } = self
            && matches!(target.as_ref(), Self::Field { .. })
        {
            return true;
        }
        self.children()
            .iter()
            .any(|child| child.assigns_to_a_field())
    }

    /// How many sequences this walks. More than one is a `Flatten` or a `Zip`,
    /// whose step depends on where the other sequence had got to.
    fn sequences_traversed(&self) -> usize {
        let here = usize::from(matches!(
            self,
            Self::Traverse { .. }
                | Self::Transform { .. }
                | Self::Sift { .. }
                | Self::Retain { .. }
        ));
        here + self
            .children()
            .iter()
            .map(|child| child.sequences_traversed())
            .sum::<usize>()
    }
}

/// What one step of a traversal hands back, when it hands back anything.
enum OneStep {
    /// The step produced a value from a computation, as `filter_map` does.
    Sifted(Form),
    /// The step kept the element it was given, as `filter` does.
    Retained(Form),
}

fn is_none_variant(form: &Form) -> bool {
    matches!(form, Form::Variant { name, payload } if name.rsplit("::").next() == Some("None") && payload.is_empty())
}

/// `Some(value)`, however the return is spelled, yielding what it carries.
fn yielded_value(form: &Form) -> Option<&Form> {
    let form = match form {
        Form::Return(inner) => inner.as_ref(),
        other => other,
    };
    match form {
        Form::Variant { name, payload } if name.rsplit("::").next() == Some("Some") => {
            match payload.as_slice() {
                [only] => Some(only),
                _ => None,
            }
        }
        _ => None,
    }
}

fn one_step_yield(body: &Form) -> Option<OneStep> {
    match body {
        // `match f(x) { None => (), Some(v) => return Some(v) }` — the step
        // produces a value and drops the element when it produces none.
        Form::Select { scrutinee, arms } => {
            let [empty, carried] = arms.as_slice() else {
                return None;
            };
            let (empty, carried) = match (&empty.pattern, &carried.pattern) {
                (Pattern::Variant { name, .. }, _) if name.rsplit("::").next() == Some("None") => {
                    (empty, carried)
                }
                (_, Pattern::Variant { name, .. }) if name.rsplit("::").next() == Some("None") => {
                    (carried, empty)
                }
                _ => return None,
            };
            if !is_unit(&empty.body) {
                return None;
            }
            let Pattern::Variant { name, parts } = &carried.pattern else {
                return None;
            };
            if name.rsplit("::").next() != Some("Some") {
                return None;
            }
            let [Pattern::Binding(bound)] = parts.as_slice() else {
                return None;
            };
            // the arm must hand back exactly what it just unwrapped, or the
            // step is doing something this rewrite does not describe
            match yielded_value(&carried.body)? {
                Form::Local(local) if local == bound => {
                    Some(OneStep::Sifted(scrutinee.as_ref().clone()))
                }
                _ => None,
            }
        }
        // `if p(x) { return Some(x) }` — the step keeps the element unchanged.
        Form::Branch {
            condition,
            consequence,
            alternative,
        } => {
            if alternative.as_ref().is_some_and(|other| !is_unit(other)) {
                return None;
            }
            yielded_value(consequence)?;
            Some(OneStep::Retained(condition.as_ref().clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod lifting {
    use super::*;
    use crate::Arm;

    fn none() -> Form {
        Form::Variant {
            name: "None".to_owned(),
            payload: vec![],
        }
    }

    fn some(payload: Form) -> Form {
        Form::Variant {
            name: "Some".to_owned(),
            payload: vec![payload],
        }
    }

    /// `(do (traverse f0 v1 BODY) (None))`, the shape a terminal search takes.
    fn search(body: Form) -> Form {
        Form::Sequence(vec![
            Form::Traverse {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(body),
                direction: Direction::Forward,
            },
            none(),
        ])
    }

    /// `match f(x) { None => (), Some(v) => return Some(v) }`
    fn produced() -> Form {
        Form::select(
            Form::Call {
                callee: Box::new(Form::Free(1)),
                arguments: vec![Form::Local(1)],
            },
            vec![
                Arm {
                    pattern: Pattern::Variant {
                        name: "None".to_owned(),
                        parts: vec![],
                    },
                    body: Form::Literal,
                },
                Arm {
                    pattern: Pattern::Variant {
                        name: "Some".to_owned(),
                        parts: vec![Pattern::Binding(2)],
                    },
                    body: Form::Return(Box::new(some(Form::Local(2)))),
                },
            ],
        )
    }

    #[test]
    fn a_search_that_produces_values_lifts_to_a_sift() {
        let lifted = search(produced()).lifted_from_one_step();
        assert_eq!(
            lifted,
            Some(Form::Sift {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(Form::Call {
                    callee: Box::new(Form::Free(1)),
                    arguments: vec![Form::Local(1)],
                }),
            }),
            "FilterMap::next delegates to find_map, so one step of it is the sift"
        );
    }

    #[test]
    fn a_search_that_keeps_elements_lifts_to_a_retain() {
        let kept = Form::Branch {
            condition: Box::new(Form::Call {
                callee: Box::new(Form::Free(1)),
                arguments: vec![Form::Local(1)],
            }),
            consequence: Box::new(Form::Return(Box::new(some(Form::Local(1))))),
            alternative: None,
        };
        let lifted = search(kept).lifted_from_one_step();
        assert!(
            matches!(lifted, Some(Form::Retain { .. })),
            "a step that hands back the element it was given is a filter: {lifted:?}"
        );
    }

    /// The lift is licensed by the caller, not by the shape alone. A search that
    /// is genuinely a search — `find_map` — has this same form, and derivation
    /// must be the thing that decides, so the rewrite stays available to it.
    #[test]
    fn anything_that_is_not_one_step_is_declined() {
        assert_eq!(Form::Free(0).lifted_from_one_step(), None);
        // a traversal that does not end by reporting exhaustion
        assert_eq!(
            Form::Sequence(vec![
                Form::Traverse {
                    sequence: Box::new(Form::Free(0)),
                    item: Box::new(Pattern::Binding(1)),
                    body: Box::new(produced()),
                    direction: Direction::Forward,
                },
                Form::Literal,
            ])
            .lifted_from_one_step(),
            None
        );
    }

    /// The lifted form names NOTHING, and that is why it cannot be reported.
    ///
    /// `is_reportable` requires `anchors >= 2` and `anchors >= holes`, because a
    /// form built only from shape matches any code of that shape — the
    /// `Option::map_or` failure that fired nine hundred times across five
    /// hundred crates. `(sift f0 v1 (call f1 v1))` is pure shape: no type, no
    /// method, no operator, no constant. Lifting `filter_map` is correct and
    /// makes it unreportable in the same stroke, which is why item 3's clippy
    /// points never arrived. Asserted here so the tension is not rediscovered.
    #[test]
    fn the_lifted_form_names_nothing() {
        let lifted = search(produced())
            .lifted_from_one_step()
            .expect("the sift shape lifts");
        assert_eq!(lifted.anchors(), 0, "a bare sift over holes names nothing");
        assert!(
            lifted.anchors() < lifted.holes(),
            "more open than named: {} anchors, {} holes",
            lifted.anchors(),
            lifted.holes()
        );
    }

    /// Fails closed. `Flatten`, `Peekable` and `Chunks` carry state between
    /// calls, so no one step of them stands for the whole operation.
    #[test]
    fn a_stateful_step_is_refused() {
        let remembers = Form::Sequence(vec![
            Form::Assign {
                operator: "=".to_owned(),
                target: Box::new(Form::Field {
                    value: Box::new(Form::Free(0)),
                    name: "index".to_owned(),
                }),
                value: Box::new(Form::Number("0".to_owned())),
            },
            produced(),
        ]);
        assert_eq!(search(remembers).lifted_from_one_step(), None);

        let two_sequences = Form::Sequence(vec![
            Form::Traverse {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(Form::Traverse {
                    sequence: Box::new(Form::Free(2)),
                    item: Box::new(Pattern::Binding(3)),
                    body: Box::new(produced()),
                    direction: Direction::Forward,
                }),
                direction: Direction::Forward,
            },
            none(),
        ]);
        assert_eq!(two_sequences.lifted_from_one_step(), None);
    }
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

    /// Unfolding a name that calls itself does not terminate.
    ///
    /// The body replacing the call contains the same call, so each pass makes
    /// the form strictly larger and the recursion in `apply_bindings` runs to
    /// the end of the stack. Found on CPython's `json/encoder.py`, which binds
    /// three mutually recursive generators inside one factory; simplifying it
    /// aborted the process rather than failing. A half-gigabyte stack did not
    /// help, which is what said it was a cycle and not depth.
    #[test]
    fn a_name_bound_to_a_body_that_calls_itself_is_left_alone() {
        let recursive = Form::Sequence(vec![
            Form::Let {
                pattern: Box::new(Pattern::Binding(0)),
                value: Box::new(lambda(
                    1,
                    Form::Call {
                        callee: Box::new(Form::Local(0)),
                        arguments: vec![Form::Local(1)],
                    },
                )),
            },
            Form::Call {
                callee: Box::new(Form::Local(0)),
                arguments: vec![Form::Free(9)],
            },
        ]);
        assert_eq!(
            recursive.simplify(),
            recursive.simplify(),
            "it terminates, which is the whole assertion"
        );
        assert!(
            recursive.simplify().to_string().contains("let"),
            "the binding survives: {}",
            recursive.simplify()
        );
    }

    /// The same, one step further out: two names that call each other.
    #[test]
    fn names_bound_to_bodies_that_call_each_other_are_left_alone() {
        let mutual = Form::Sequence(vec![
            Form::Let {
                pattern: Box::new(Pattern::Binding(0)),
                value: Box::new(lambda(
                    2,
                    Form::Call {
                        callee: Box::new(Form::Local(1)),
                        arguments: vec![Form::Local(2)],
                    },
                )),
            },
            Form::Let {
                pattern: Box::new(Pattern::Binding(1)),
                value: Box::new(lambda(
                    3,
                    Form::Call {
                        callee: Box::new(Form::Local(0)),
                        arguments: vec![Form::Local(3)],
                    },
                )),
            },
            Form::Call {
                callee: Box::new(Form::Local(0)),
                arguments: vec![Form::Free(9)],
            },
        ]);
        assert!(mutual.simplify().to_string().contains("let"));
    }

    /// A binding that merely CALLS a safe one still unfolds, because
    /// substituting through an acyclic group terminates. Refusing those too
    /// would have been a cheaper fix and a worse one.
    #[test]
    fn a_chain_of_bindings_that_does_not_cycle_still_unfolds() {
        let chained = Form::Sequence(vec![
            Form::Let {
                pattern: Box::new(Pattern::Binding(0)),
                value: Box::new(lambda(1, Form::Local(1))),
            },
            Form::Let {
                pattern: Box::new(Pattern::Binding(2)),
                value: Box::new(lambda(
                    3,
                    Form::Call {
                        callee: Box::new(Form::Local(0)),
                        arguments: vec![Form::Local(3)],
                    },
                )),
            },
            Form::Call {
                callee: Box::new(Form::Local(2)),
                arguments: vec![Form::Free(9)],
            },
        ]);
        assert_eq!(chained.simplify().to_string(), "f9");
    }

    #[test]
    fn recovering_a_break_value_yields_some_or_none() {
        let traversal = Form::Method {
            name: "break_value".to_owned(),
            receiver: Box::new(Form::Traverse {
                direction: Direction::Forward,
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
    fn taking_the_first_of_what_was_retained_is_a_search() {
        let first_of_filtered = Form::Method {
            name: "first".to_owned(),
            receiver: Box::new(Form::Retain {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(Form::Method {
                    name: "is_ready".to_owned(),
                    receiver: Box::new(Form::Local(1)),
                    arguments: Vec::new(),
                }),
            }),
            arguments: Vec::new(),
        };
        assert_eq!(
            first_of_filtered.simplify().to_string(),
            "(do (traverse f0 v1 (branch (method is_ready v1) \
             (return (variant Some v1)))) (variant None))",
            "a search that yields the element or nothing yields an optional, \
             which is the shape a language with an option type writes directly"
        );
    }

    /// Searching backwards is not searching forwards.
    ///
    /// `filter(p)` then taking the LAST survivor answers a different question,
    /// and rewriting it to a forward search would name the wrong API. It is
    /// left alone, so it fails to match rather than matching wrongly.
    #[test]
    fn taking_the_last_of_what_was_retained_is_left_alone() {
        let end_of_filtered = |end: &str| Form::Method {
            name: end.to_owned(),
            receiver: Box::new(Form::Retain {
                sequence: Box::new(Form::Free(0)),
                item: Box::new(Pattern::Binding(1)),
                body: Box::new(Form::Local(1)),
            }),
            arguments: Vec::new(),
        };
        let last = end_of_filtered("last").simplify();
        assert_ne!(
            last,
            end_of_filtered("first").simplify(),
            "a backwards search must not reduce to a forwards one"
        );
        assert!(
            last.to_string().contains("traverse-back"),
            "a backwards search walks the other way: {last}"
        );
    }

    #[test]
    fn a_form_with_no_applicable_law_is_unchanged() {
        let plain = Form::Traverse {
            direction: Direction::Forward,
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
