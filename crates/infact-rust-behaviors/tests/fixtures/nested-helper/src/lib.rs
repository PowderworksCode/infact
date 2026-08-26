pub trait Walk {
    fn items(&self) -> Vec<i32>;

    /// A provided method with a real body and a name nothing else declares —
    /// except a helper nested inside a sibling, below.
    fn total(&self) -> i32 {
        let mut sum = 0;
        for value in self.items() {
            sum += value;
        }
        sum
    }

    /// Declares its own `total`, the way `Iterator::max_by` declares its own
    /// `fold`.
    fn biggest(&self) -> i32 {
        fn total(left: i32, right: i32) -> i32 {
            if left > right { left } else { right }
        }
        let mut best = 0;
        for value in self.items() {
            best = total(best, value);
        }
        best
    }
}
