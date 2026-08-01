//! One effectful call, written four ways. Syntax tells them apart.
use std::fs;
use std::fs::read_to_string;

pub fn qualified() {
    let _ = std::fs::read("a");
}

pub fn via_module() {
    let _ = fs::read("b");
}

pub fn via_item() {
    let _ = read_to_string("c");
}

pub fn caller() {
    via_module();
}

pub fn pure() -> usize {
    42
}
