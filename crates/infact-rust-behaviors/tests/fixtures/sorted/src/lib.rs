pub fn natural(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values = input.collect::<Vec<_>>();
    values.sort();
    values
}

pub fn comparator(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values: Vec<_> = input.collect();
    values.sort_by(|left, right| left.len().cmp(&right.len()));
    values
}

pub fn key(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values = input.collect::<Vec<_>>();
    values.sort_by_key(|value| value.len());
    values
}

pub fn unstable(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values = input.collect::<Vec<_>>();
    values.sort_unstable();
    values
}

pub fn unstable_comparator(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values = input.collect::<Vec<_>>();
    values.sort_unstable_by(|left, right| left.len().cmp(&right.len()));
    values
}

pub fn unstable_key(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values = input.collect::<Vec<_>>();
    values.sort_unstable_by_key(|value| value.len());
    values
}

pub fn not_adjacent(input: impl Iterator<Item = String>) -> Vec<String> {
    let mut values = input.collect::<Vec<_>>();
    values.push(String::new());
    values.sort();
    values
}
