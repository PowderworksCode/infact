//! The same method names on a type that is not the standard library's.
//!
//! Matching is by method name, so this is where name-based matching is most
//! likely to be wrong: suggesting an iterator API for code operating on a
//! bespoke container would be a false positive.

/// A container of the crate's own, with methods that happen to share names.
#[derive(Default)]
pub struct Registry {
    names: Vec<String>,
}

pub struct Slot;

impl Slot {
    pub fn or_default(&self) -> &Self {
        self
    }
    pub fn push(&self, _value: u32) {}
}

impl Registry {
    pub fn entry(&self, _key: String) -> Slot {
        Slot
    }
    pub fn sort(&mut self) {
        self.names.sort();
    }
}

// a bespoke registry, not a map; `into_group_map` would be wrong advice
// expect: none
pub fn fill_registry(pairs: Vec<(String, u32)>) -> Registry {
    let mut registry = Registry::default();
    for (key, value) in pairs {
        registry.entry(key).or_default().push(value);
    }
    registry
}

// a bespoke registry that sorts itself; `sorted` would be wrong advice
// expect: none
pub fn sorted_registry(names: Vec<String>) -> Registry {
    let mut registry = Registry { names };
    registry.sort();
    registry
}
