//! A `Cursor` over pairs, which yields the first of each and has a `next`.

pub struct Cursor<'a, K> {
    inner: Pairs<'a, K>,
}

impl<'a, K> Cursor<'a, K> {
    pub fn next(&mut self) -> Option<&'a K> {
        self.inner.next().map(|(k, _)| k)
    }
}

pub fn keys<'a, K>(pairs: Pairs<'a, K>) -> Cursor<'a, K> {
    Cursor { inner: pairs }
}
