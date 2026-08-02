/// A documented enum, which is what idiomatic Rust looks like.
///
/// `line_comment` is a NAMED child of `enum_variant_list`, so a recognizer
/// that requires every named child to be an `enum_variant` sees no unit enum
/// here and reports nothing at all.
pub enum Documented {
    /// The first one.
    FirstValue,
    /// The second one.
    SecondValue,
}

impl Documented {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstValue => "first-value",
            Self::SecondValue => "second-value",
        }
    }
}

impl std::fmt::Display for Documented {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(formatter)
    }
}
