//! Allocation, and things that look like it.
use std::collections::HashMap;
use std::sync::Arc;

pub fn allocates_directly() -> Vec<u8> {
    Vec::with_capacity(16)
}

pub fn allocates_through_a_macro(name: &str) -> String {
    format!("hello {name}")
}

pub fn allocates_through_a_method(values: &[u8]) -> Vec<u8> {
    values.to_vec()
}

pub fn caller() -> Vec<u8> {
    allocates_directly()
}

pub fn distant_caller() -> Vec<u8> {
    caller()
}

/// An empty container reaches no allocator, so neither of these is an origin.
pub fn allocates_nothing() -> Vec<u8> {
    let empty: Vec<u8> = Vec::new();
    let _also_empty: Vec<u8> = vec![];
    empty
}

/// Cloning an `Arc` bumps a count. Syntax cannot see that, so it must not say.
pub fn clones_a_handle(handle: &Arc<HashMap<String, u8>>) -> Arc<HashMap<String, u8>> {
    handle.clone()
}
