//! `Iterator::find` as the standard library writes it, and nothing else.
//!
//! The point of the fixture is that this is *not* a loop: it is a fold over
//! `ControlFlow` threaded through a locally defined helper, which is the shape
//! the real implementation has and the shape no amount of tree comparison
//! relates to the loop a caller writes.

pub trait Iterator {
    type Item;

    fn find<P>(&mut self, predicate: P) -> Option<Self::Item>
    where
        Self: Sized,
        P: FnMut(&Self::Item) -> bool,
    {
        #[inline]
        fn check<T>(mut predicate: impl FnMut(&T) -> bool) -> impl FnMut((), T) -> ControlFlow<T> {
            move |(), x| {
                if predicate(&x) {
                    ControlFlow::Break(x)
                } else {
                    ControlFlow::Continue(())
                }
            }
        }

        self.try_fold((), check(predicate)).break_value()
    }
}
