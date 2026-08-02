//! Reading a Rust path the way a compiler wrote it.
//!
//! A resolved destination is not the path anyone typed. It carries the generic
//! arguments the call was monomorphized with, and a trait method arrives as a
//! qualified form naming the type it was called on. Both are information the
//! syntax never had, and both have to be taken apart before a path can be
//! compared with one a human declared.

/// The path with its turbofish arguments removed.
///
/// A resolved destination writes out what the caller left implicit, so
/// `std::fs::read` arrives as `std::fs::read::<&str>`. A catalog records the
/// declared path, and matching one against the other means dropping the
/// substitution. Only `::<…>` is removed: a leading `<Type as Trait>` is the
/// path's structure rather than an argument to it.
pub(crate) fn without_turbofish(path: &str) -> String {
    let mut plain = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(offset) = rest.find("::<") {
        let group = &rest[offset + "::".len()..];
        let Some(end) = close(group) else { break };
        // `::<impl Trait for Type>` names the impl a method was found in. It
        // is the path's structure, and dropping it would lose the type.
        if group.starts_with("<impl ") {
            plain.push_str(&rest[..offset + "::".len() + end]);
        } else {
            plain.push_str(&rest[..offset]);
        }
        rest = &group[end..];
    }
    plain.push_str(rest);
    plain
}

/// The path with every generic argument group removed.
///
/// Unlike [`without_turbofish`] this does not preserve a qualified form, so it
/// is for deciding what a path *names* rather than for reconstructing one.
pub(crate) fn without_generics(path: &str) -> String {
    let mut plain = String::with_capacity(path.len());
    let mut depth = 0usize;
    for character in path.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(character),
            _ => {}
        }
    }
    plain
}

/// The type a qualified method was called on.
///
/// A compiler writes an impl method two ways. `<Vec<u8> as Clone>::clone` names
/// the type first; `std::str::<impl ToOwned for str>::to_owned` names the
/// module the impl sits in and puts the type after `for`. Both say which type
/// the method ran on, which for a method like `clone` is the entire question.
pub(crate) fn qualified_self(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix('<') {
        return before_separator(rest, " as ");
    }
    let opening = path.find("::<impl ")?;
    let group = &path[opening + "::".len()..];
    let implemented = group[1..close(group)?.saturating_sub(1)].strip_prefix("impl ")?;
    // an inherent impl names only the type; a trait impl names it after `for`
    Some(match before_separator(implemented, " for ") {
        Some(trait_path) => &implemented[trait_path.len() + " for ".len()..],
        None => implemented,
    })
}

/// The type constructor a type expression names, without its arguments or
/// module path: `std::sync::Arc<String>` is an `Arc`.
pub(crate) fn type_head(type_expression: &str) -> &str {
    let base = type_expression
        .split('<')
        .next()
        .unwrap_or(type_expression)
        .trim();
    base.rsplit("::").next().unwrap_or(base)
}

/// Whether a type expression mentions a particular constructor anywhere in it,
/// so that `Result<Vec<u8>, Error>` is seen to contain a `Vec`.
pub(crate) fn mentions_type(type_expression: &str, head: &str) -> bool {
    type_expression
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|part| part == head)
}

/// The offset just past the `<…>` group a string opens with.
fn close(opening: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in opening.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// What comes before a separator, at the top level of a type expression.
fn before_separator<'a>(value: &'a str, separator: &str) -> Option<&'a str> {
    let mut depth = 0usize;
    for (index, character) in value.char_indices() {
        match character {
            '<' | '[' | '(' => depth += 1,
            '>' | ']' | ')' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            _ => {}
        }
        if depth == 0 && value[index..].starts_with(separator) {
            return Some(&value[..index]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resolved_path_loses_the_arguments_it_was_monomorphized_with() {
        assert_eq!(without_turbofish("std::fs::read::<&str>"), "std::fs::read");
        assert_eq!(
            without_turbofish("std::vec::Vec::<u8>::with_capacity"),
            "std::vec::Vec::with_capacity"
        );
        assert_eq!(
            without_turbofish("<C as Iterator>::collect::<std::vec::Vec<u8>>"),
            "<C as Iterator>::collect"
        );
        assert_eq!(without_turbofish("std::fmt::format"), "std::fmt::format");
    }

    /// A qualified path is structure, not an argument, and must survive.
    ///
    /// The `::<impl …>` form is the trap: it opens exactly like a turbofish,
    /// and removing it takes the receiver's type with it, which is the one
    /// thing the resolved destination was wanted for.
    #[test]
    fn a_qualified_path_is_not_a_turbofish() {
        assert_eq!(
            without_turbofish("<std::string::String as std::clone::Clone>::clone"),
            "<std::string::String as std::clone::Clone>::clone"
        );
        assert_eq!(
            without_turbofish("std::str::<impl std::borrow::ToOwned for str>::to_owned"),
            "std::str::<impl std::borrow::ToOwned for str>::to_owned"
        );
        // both at once: the impl survives and the argument list does not
        assert_eq!(
            without_turbofish("core::slice::<impl [u8]>::to_vec::<std::alloc::Global>"),
            "core::slice::<impl [u8]>::to_vec"
        );
    }

    #[test]
    fn a_qualified_method_names_the_type_it_ran_on() {
        assert_eq!(
            qualified_self("<std::string::String as std::clone::Clone>::clone"),
            Some("std::string::String")
        );
        assert_eq!(
            qualified_self("<std::sync::Arc<std::string::String> as std::clone::Clone>::clone"),
            Some("std::sync::Arc<std::string::String>")
        );
        assert_eq!(
            qualified_self("<i32 as std::borrow::ToOwned>::to_owned"),
            Some("i32")
        );
    }

    /// The other spelling: the impl's module first, the type after `for`.
    #[test]
    fn an_impl_written_in_a_module_still_names_its_type() {
        assert_eq!(
            qualified_self("std::str::<impl std::borrow::ToOwned for str>::to_owned"),
            Some("str")
        );
        assert_eq!(
            qualified_self("core::slice::<impl [u8]>::iter"),
            Some("[u8]")
        );
    }

    #[test]
    fn a_plain_path_qualifies_nothing() {
        assert_eq!(qualified_self("std::fs::read"), None);
        assert_eq!(qualified_self("std::vec::Vec::<u8>::new"), None);
    }

    #[test]
    fn a_type_reduces_to_its_constructor() {
        assert_eq!(type_head("std::sync::Arc<std::string::String>"), "Arc");
        assert_eq!(type_head("std::string::String"), "String");
        assert_eq!(type_head("i32"), "i32");
        assert_eq!(type_head("str"), "str");
    }

    #[test]
    fn a_container_is_found_wherever_it_is_nested() {
        assert!(mentions_type(
            "std::result::Result<std::vec::Vec<u8>, E>",
            "Vec"
        ));
        assert!(!mentions_type("std::result::Result<(), E>", "Vec"));
        assert!(!mentions_type("()", "Vec"));
    }
}
