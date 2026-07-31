pub fn total_prices(prices: &[u32]) -> u32 {
    let mut total = 0;
    for price in prices {
        total += price * 2;
    }
    total
}
