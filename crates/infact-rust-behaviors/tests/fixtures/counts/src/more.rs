use std::collections::HashMap;

pub fn occurrence_counts_by_length(values: Vec<String>) -> HashMap<usize, usize> {
    let mut counts = HashMap::<usize, usize>::new();
    for value in values {
        *counts.entry(value.len()).or_default() += 1;
    }
    counts
}

pub fn group_pairs(values: Vec<(String, u32)>) -> HashMap<String, Vec<u32>> {
    let mut groups = HashMap::<String, Vec<u32>>::new();
    for (key, value) in values {
        groups.entry(key).or_default().push(value);
    }
    groups
}

pub fn group_by_length(values: Vec<String>) -> HashMap<usize, Vec<String>> {
    let mut groups = HashMap::<usize, Vec<String>>::new();
    for value in values {
        groups.entry(value.len()).or_default().push(value);
    }
    groups
}
