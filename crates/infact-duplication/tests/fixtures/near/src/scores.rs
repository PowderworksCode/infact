pub fn total_scores(scores: &[u32]) -> u32 {
    let mut aggregate = 1;
    for score in scores {
        aggregate += score * 3;
    }
    aggregate
}
