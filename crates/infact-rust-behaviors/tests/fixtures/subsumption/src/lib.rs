pub fn mapped(value: Option<i32>) -> Option<i32> {
    match value {
        Some(inner) => Some(double(inner)),
        None => None,
    }
}

fn double(value: i32) -> i32 {
    value * 2
}
