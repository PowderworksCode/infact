pub fn first_matching(values: &[u32], target: u32) -> Option<u32> {
    for value in values {
        if value == target {
            return Some(value);
        }
    }
    None
}
