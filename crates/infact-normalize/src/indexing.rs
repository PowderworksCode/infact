//! Reading and rewriting the positions a body indexes.
//!
//! Recognizing a counted loop as a walk means proving the body indexes exactly
//! one sequence at the loop's positions, then rewriting those reads as the
//! element itself. That is a search and a substitution over the whole form,
//! and it is only ever asked for by the loop laws in `simplify`, so it sits
//! beside them rather than in the form definition.

use crate::{Form, is_next_position};

impl Form {
    /// The one sequence a body indexes at every named position.
    ///
    /// A loop bound is often a variable rather than the sequence's own length —
    /// `for i in 0..n` far more often than `for i in 0..v.len()` — so the span
    /// does not always say what is being walked. The body does: whatever it
    /// reads at those positions is the sequence, and it has to be exactly one
    /// of them, or the loop is walking positions into two things at once and is
    /// not a walk over either.
    pub(crate) fn sole_indexed_sequence(&self, positions: &[u32]) -> Option<&Self> {
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

    /// The largest binding number anything here introduces or mentions.
    ///
    /// A rewrite that turns one name into two needs a number for the second,
    /// and reusing one already in play would silently identify two different
    /// values. Renaming makes the numbers canonical afterwards, so any unused
    /// one will do.
    pub(crate) fn highest_binding(&self) -> Option<u32> {
        let here = match self {
            Self::Local(index) => Some(*index),
            _ => None,
        };
        self.children()
            .into_iter()
            .filter_map(Self::highest_binding)
            .chain(here)
            .max()
    }

    /// Whether a body reads a sequence only by indexing it at a position and
    /// its successor.
    ///
    /// The adjacent-pairs licence, and narrower than it looks: `v[i]` and
    /// `v[i + 1]` are the only two readings allowed, so `v[i + 2]`, a bare `i`,
    /// or any other use of `v` declines. Without that, a loop that happens to
    /// read a neighbour among other things would be reported as a walk over
    /// neighbours, which is not what it does.
    pub(crate) fn adjacent_only(&self, sequence: &Self, index: u32) -> bool {
        if let Self::Index {
            sequence: indexed,
            position,
        } = self
            && indexed.as_ref() == sequence
            && (**position == Self::Local(index) || is_next_position(position, index))
        {
            return true;
        }
        if self == sequence || *self == Self::Local(index) {
            return false;
        }
        self.children()
            .into_iter()
            .all(|child| child.adjacent_only(sequence, index))
    }

    /// The same body with each neighbour reading replaced by its element.
    pub(crate) fn with_adjacent_elements(
        &self,
        sequence: &Self,
        index: u32,
        successor: u32,
    ) -> Self {
        if let Self::Index {
            sequence: indexed,
            position,
        } = self
            && indexed.as_ref() == sequence
        {
            if **position == Self::Local(index) {
                return Self::Local(index);
            }
            if is_next_position(position, index) {
                return Self::Local(successor);
            }
        }
        self.map_children(&|child| child.with_adjacent_elements(sequence, index, successor))
    }

    /// Whether a body reads a sequence only by indexing it at named positions.
    ///
    /// This is the licence to forget the index. `for i in 0..v.len()` visits
    /// each element of `v` only when `i` is used for nothing but `v[i]` and `v`
    /// is reached no other way: `v[i + 1]` looks at a different element,
    /// `w[i]` at a different sequence, and `v.swap(i, j)` at the sequence
    /// itself. Each of those makes the loop something other than an element
    /// visit, and each fails this test.
    pub(crate) fn indexed_only(&self, sequence: &Self, positions: &[u32]) -> bool {
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
    pub(crate) fn writes_indexed(&self, sequence: &Self) -> bool {
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
    pub(crate) fn with_indexed_elements(&self, sequence: &Self, positions: &[u32]) -> Self {
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
}
