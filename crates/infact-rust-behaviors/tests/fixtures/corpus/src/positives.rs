//! Genuine reimplementations. Each should be recognized as the named API.
use std::collections::HashMap;

// expect: itertools::Itertools::counts
pub fn tally(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts
}

// expect: itertools::Itertools::into_group_map
pub fn bucket(pairs: Vec<(String, u32)>) -> HashMap<String, Vec<u32>> {
    let mut groups = HashMap::new();
    for (key, value) in pairs {
        groups.entry(key).or_default().push(value);
    }
    groups
}

// expect: itertools::Itertools::sorted
pub fn ordered(values: Vec<u32>) -> Vec<u32> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort();
    ordered
}

// expect: itertools::Itertools::sorted_by
pub fn ordered_by(values: Vec<u32>) -> Vec<u32> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort_by(|a, b| b.cmp(a));
    ordered
}

// expect: itertools::Itertools::sorted_by_key
pub fn ordered_by_key(values: Vec<String>) -> Vec<String> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort_by_key(|value| value.len());
    ordered
}

// expect: itertools::Itertools::sorted_unstable
pub fn ordered_unstable(values: Vec<u32>) -> Vec<u32> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort_unstable();
    ordered
}

// expect: itertools::Itertools::sorted_unstable_by
pub fn ordered_unstable_by(values: Vec<u32>) -> Vec<u32> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort_unstable_by(|a, b| b.cmp(a));
    ordered
}

// expect: itertools::Itertools::sorted_unstable_by_key
pub fn ordered_unstable_by_key(values: Vec<String>) -> Vec<String> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort_unstable_by_key(|value| value.len());
    ordered
}

// the result is consumed rather than returned, which is the common shape
// expect: itertools::Itertools::counts
pub fn distinct(values: Vec<String>) -> usize {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts.len()
}

// written with a combinator instead of a loop
// expect: itertools::Itertools::counts
pub fn tally_with_combinator(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    values.into_iter().for_each(|value| {
        *counts.entry(value).or_default() += 1;
    });
    counts
}

// an unrelated declaration sits between the accumulator and the loop
// expect: itertools::Itertools::into_group_map
pub fn grouped_around_other_work(pairs: Vec<(String, u32)>) -> HashMap<String, Vec<u32>> {
    let mut groups = HashMap::new();
    let label = String::from("run");
    let _ = label;
    for (key, value) in pairs {
        groups.entry(key).or_default().push(value);
    }
    groups
}
