//! A loop that skips the empty entries is a traversal of the present ones.
pub fn report(entries: &[Option<u16>]) {
    for entry in entries {
        if let Some(value) = entry {
            record(*value);
        }
    }
}

fn record(_value: u16) {}
