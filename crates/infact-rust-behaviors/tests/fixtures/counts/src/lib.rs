use std::collections::HashMap;

pub fn occurrence_counts(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::<String, usize>::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts
}
