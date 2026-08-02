//! `if let` with an `else`, where the library names the case this writes `_`.
pub fn port(configured: Option<u16>) -> u16 {
    if let Some(value) = configured { value } else { 8080 }
}
