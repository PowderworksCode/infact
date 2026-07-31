pub fn second(items: &[u32]) -> u32 {
    // Formatting and comments are not syntax tokens.
    let mut total=0;

    for item in items
    {
        total+=item;
    }
    total
}
