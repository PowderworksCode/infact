//! Code that looks like a library behavior but is not one. Every match here
//! would be a false positive, and precision is what makes these findings worth
//! reading at all.
use std::collections::HashMap;

// sums a value per key; that is not counting occurrences
// expect: none
pub fn sum_per_key(pairs: Vec<(String, u32)>) -> HashMap<String, u32> {
    let mut totals = HashMap::new();
    for (key, value) in pairs {
        *totals.entry(key).or_default() += value;
    }
    totals
}

// counts in twos, so the result is not a count
// expect: none
pub fn count_by_twos(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(value).or_default() += 2;
    }
    counts
}

// keeps only the last value per key instead of collecting them
// expect: none
pub fn last_per_key(pairs: Vec<(String, u32)>) -> HashMap<String, u32> {
    let mut latest = HashMap::new();
    for (key, value) in pairs {
        latest.insert(key, value);
    }
    latest
}

// stops early, so it does not visit the whole sequence
// expect: none
pub fn count_until_empty(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for value in values {
        if value.is_empty() {
            break;
        }
        *counts.entry(value).or_default() += 1;
    }
    counts
}

// skips some items, so it is a filtered count
// expect: none
pub fn count_non_empty(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for value in values {
        if value.is_empty() {
            continue;
        }
        *counts.entry(value).or_default() += 1;
    }
    counts
}

// sorts and then removes duplicates. The sort really is `sorted`, and
// `.sorted().dedup()` is the refactoring, so reporting it is correct.
// expect: itertools::Itertools::sorted
pub fn sorted_distinct(values: Vec<u32>) -> Vec<u32> {
    let mut ordered = Vec::from_iter(values);
    ordered.sort();
    ordered.dedup();
    ordered
}

// sorts in place without collecting, so there is nothing to replace
// expect: none
pub fn sort_in_place(values: &mut [u32]) {
    values.sort();
}

// reverses rather than sorts
// expect: none
pub fn reversed(values: Vec<u32>) -> Vec<u32> {
    let mut ordered = Vec::from_iter(values);
    ordered.reverse();
    ordered
}

// collects without ordering
// expect: none
pub fn collected(values: Vec<u32>) -> Vec<u32> {
    let ordered = Vec::from_iter(values);
    ordered
}

// counts, but also logs, so the behavior is not equivalent
// expect: none
pub fn tally_and_log(values: Vec<String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for value in values {
        eprintln!("{value}");
        *counts.entry(value).or_default() += 1;
    }
    counts
}
