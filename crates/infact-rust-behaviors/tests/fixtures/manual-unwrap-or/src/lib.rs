//! The arms are written in the other order, which is not a difference.
pub fn port(configured: Option<u16>) -> u16 {
    match configured {
        None => 8080,
        Some(value) => value,
    }
}
