//! The same method on receivers that cost different things.
use std::sync::Arc;

pub fn clones_a_handle(handle: &Arc<String>) -> Arc<String> {
    handle.clone()
}

pub fn clones_a_string(value: &String) -> String {
    value.clone()
}

pub fn owns_a_str(value: &str) -> String {
    value.to_owned()
}

pub fn owns_an_integer(value: &i32) -> i32 {
    value.to_owned()
}

pub fn collects_a_vec(values: &[u8]) -> Vec<u8> {
    values.iter().copied().collect()
}

pub fn collects_nothing(values: &[u8]) -> Result<(), ()> {
    values.iter().map(|_| Ok(())).collect()
}

pub fn formats(name: &str) -> String {
    format!("hi {name}")
}
