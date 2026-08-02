//! `panic!` is not a default: this is `expect`, not `unwrap_or`.
pub fn port(configured: Option<u16>) -> u16 {
    match configured {
        Some(value) => value,
        None => panic!("no port configured"),
    }
}
