//! A loop whose test can leave the function is not `find`: no predicate a
//! caller could pass would short-circuit the whole call.

pub fn first_matching(values: &[u32]) -> Option<u32> {
    for value in values {
        if check(value).ok()? {
            return Some(value);
        }
    }
    None
}
