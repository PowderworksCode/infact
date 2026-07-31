use std::collections::HashMap;

pub fn counts_with_another_loop_effect(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::<String, usize>::new();
    for value in values {
        eprintln!("counting {value}");
        *counts.entry(value).or_default() += 1;
    }
    counts
}
