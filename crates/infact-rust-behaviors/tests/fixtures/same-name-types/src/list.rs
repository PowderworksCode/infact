//! A different `Cursor`, sharing only its name. No `inner` field, and no
//! principal method anywhere on it.

pub struct Cursor<'a, T> {
    index: usize,
    list: &'a List<T>,
}

pub fn cursor_front<'a, T>(list: &'a List<T>) -> Cursor<'a, T> {
    Cursor { index: 0, list }
}

pub fn cursor_back<'a, T>(list: &'a List<T>) -> Cursor<'a, T> {
    Cursor { index: list.len - 1, list }
}
