use std::collections::HashMap;

/// Counts, then uses the result instead of returning it.
pub fn distinct_count(values: Vec<String>) -> usize {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts.len()
}
