//! Two `Option` consumers that differ only in how much they leave open.

pub enum Option<T> {
    Some(T),
    None,
}

impl<T> Option<T> {
    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Some(value) => value,
            None => default,
        }
    }

    pub fn map_or<U, F>(self, default: U, f: F) -> U {
        match self {
            Some(value) => f(value),
            None => default,
        }
    }
}
