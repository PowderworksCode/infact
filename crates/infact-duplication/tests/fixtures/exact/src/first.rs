pub fn first(items: &[u32]) -> u32 {
    let mut total = 0;
    for item in items {
        total += item;
    }
    total
}
