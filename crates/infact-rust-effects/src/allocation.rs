//! Which operations reach the allocator, and where they are written.
//!
//! Separated from the effect analysis proper because it is a table of
//! standard-library knowledge rather than a way of propagating anything, and
//! because what belongs in it is a judgement that wants reviewing on its own.

use std::path::Path;

use tree_sitter::Node;

use infact_core::Effect;

use crate::{
    EffectSeed, Result, SyntaxCall, identifiers, node_text, path, source_span, trailing_identifier,
};

/// The seed a call makes, when the call allocates.
///
/// An allocation is an effect origin on the same terms as a catalogued call:
/// the operation itself does the work and there is nothing local to link to.
/// The caller decides this before local resolution for the same reason it
/// consults the catalog first — a standard-library container is not a
/// repository callable that happens to share a name.
pub(crate) fn call_seed(callable: u64, call: &SyntaxCall) -> Option<EffectSeed> {
    let operation = allocating_call(&call.callee)?;
    Some(EffectSeed {
        callable,
        effect: Effect::Allocate,
        origin: format!("rust:allocation:{operation}"),
        span: call.span.clone(),
    })
}

/// Seeds for the allocating macros a callable expands.
///
/// Macros are not call-shaped expressions, so they sit outside what the call
/// accounting counts. Only the allocating ones were collected, so each one is
/// already an origin.
pub(crate) fn macro_seeds(callable: u64, expansions: &[SyntaxCall]) -> Vec<EffectSeed> {
    expansions
        .iter()
        .map(|expansion| EffectSeed {
            callable,
            effect: Effect::Allocate,
            origin: format!("rust:allocation:{}", expansion.callee),
            span: expansion.span.clone(),
        })
        .collect()
}

/// Allocating macro expansions written inside one callable.
///
/// A macro is not a call expression, so the call walk never reaches it, and
/// `format!` in a loop is exactly what a caller asking about allocation wants
/// to be told about. Only the allocating ones are recorded, so what this
/// returns is already an answer rather than a list to be filtered later.
pub(crate) fn collect_allocating_macros(
    root: Node<'_>,
    node: Node<'_>,
    source: &[u8],
    path: &Path,
    output: &mut Vec<SyntaxCall>,
) -> Result<()> {
    if node != root && node.kind() == "function_item" {
        return Ok(());
    }
    if node.kind() == "macro_invocation"
        && let Some(name) = node
            .child_by_field_name("macro")
            .and_then(|name| node_text(name, source))
        && let Some(name) = trailing_identifier(name)
        && let Some(operation) = allocating_macro(name, macro_expands_arguments(node))
    {
        output.push(SyntaxCall {
            callee: operation.to_owned(),
            span: source_span(path, node)?,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_allocating_macros(root, child, source, path, output)?;
    }
    Ok(())
}

/// Whether a macro was handed anything to expand.
///
/// `vec![]` expands to `Vec::new` and reaches no allocator, for the same
/// reason `Vec::new` itself does not.
fn macro_expands_arguments(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "token_tree")
        .any(|tree| {
            let mut inner = tree.walk();
            tree.named_children(&mut inner).next().is_some()
        })
}

/// Standard-library associated functions that reach the allocator.
///
/// Naming the operation is not enough on its own. `Vec::new` and `String::new`
/// hand back a dangling pointer and touch no allocator until something is put
/// in them, while `Vec::with_capacity` allocates where it is written. A policy
/// that refuses allocation is only worth having if it tells those apart, so the
/// empty constructors are absent here deliberately rather than by oversight.
const ALLOCATING_ASSOCIATED: &[(&str, &str)] = &[
    ("Arc", "new"),
    ("Box", "new"),
    ("CString", "new"),
    ("HashMap", "with_capacity"),
    ("HashSet", "with_capacity"),
    ("OsString", "from"),
    ("PathBuf", "from"),
    ("Rc", "new"),
    ("String", "from"),
    ("String", "with_capacity"),
    ("Vec", "from"),
    ("Vec", "with_capacity"),
    ("VecDeque", "with_capacity"),
];

/// Methods that allocate whatever they are called on.
///
/// A method belongs here only when every receiver it could have allocates.
/// `clone` and `to_owned` do not qualify: cloning an `Arc` bumps a count, and
/// `to_owned` on a `Copy` type is a move. Both need the receiver's type before
/// anything can be said, syntax does not have it, and a guess would put false
/// origins inside the one relation whose value is that it can be trusted.
/// Resolved type observations are what would settle them.
const ALLOCATING_METHODS: &[&str] = &["collect", "to_string", "to_vec"];

/// The standard-library allocation a callee performs, named canonically.
///
/// The same operation arrives spelled several ways — `Vec::with_capacity` as
/// written, `alloc::vec::Vec::<T>::with_capacity` once a compiler has resolved
/// it — so the decision is made on the trailing names, which every spelling
/// shares.
pub(crate) fn allocating_call(callee: &str) -> Option<String> {
    let plain = path::without_generics(callee);
    let identifiers = identifiers(&plain);
    let (container, function) = match identifiers.as_slice() {
        [.., container, function] => (Some(*container), *function),
        [function] => (None, *function),
        [] => return None,
    };
    if ALLOCATING_METHODS.contains(&function) {
        return Some(function.to_owned());
    }
    let container = container?;
    ALLOCATING_ASSOCIATED
        .iter()
        .find(|(type_name, name)| *type_name == container && *name == function)
        .map(|(type_name, name)| format!("{type_name}::{name}"))
}

fn allocating_macro(name: &str, expands_arguments: bool) -> Option<&'static str> {
    match name {
        "format" => Some("format!"),
        "vec" if expands_arguments => Some("vec!"),
        _ => None,
    }
}

/// Types whose `clone` copies a buffer, and types whose `clone` does not.
///
/// This is the question syntax could not answer. `Arc` and `Rc` bump a count;
/// a container duplicates what it holds. Only a resolved destination names the
/// receiver, so only the observed path can ask. A type in neither list is not
/// guessed at: a user type's `Clone` may do anything.
const CLONE_ALLOCATES: &[&str] = &[
    "BTreeMap", "BTreeSet", "Box", "CString", "HashMap", "HashSet", "OsString", "PathBuf",
    "String", "Vec", "VecDeque",
];

/// Borrowed types whose owned form is heap-allocated.
///
/// `to_owned` is `Clone` read from the other side: the destination names what
/// was borrowed, and `str` becoming `String` allocates where `i32` becoming
/// `i32` is a move.
const TO_OWNED_ALLOCATES: &[&str] = &["CStr", "OsStr", "Path", "str"];

/// Containers whose construction by `collect` reaches the allocator.
const COLLECT_ALLOCATES: &[&str] = &[
    "BTreeMap", "BTreeSet", "Box", "HashMap", "HashSet", "String", "Vec", "VecDeque",
];

/// Free functions that allocate, named exactly as a compiler resolves them.
///
/// `format!` reaches the allocator through `std::fmt::format`, so on this path
/// the macro is already accounted for as an ordinary call.
const ALLOCATING_FUNCTIONS: &[&str] = &["std::fmt::format", "alloc::fmt::format"];

/// The allocation a resolved destination performs.
///
/// A compiler has already said which type a method ran on and which container
/// a `collect` was building, so this decides on evidence where
/// [`allocating_call`] can only decide on a name. Anything it cannot place is
/// declined rather than assumed, and the syntax path's answer still stands
/// underneath.
pub(crate) fn allocating_destination(destination: &str) -> Option<String> {
    let plain = path::without_turbofish(destination);
    if ALLOCATING_FUNCTIONS.contains(&plain.as_str()) {
        return Some(plain);
    }
    let operation = trailing_identifier(&plain)?;
    // a resolved `collect` arrives qualified by the iterator it consumed, so
    // the container it builds is the turbofish rather than the receiver
    if operation == "collect" {
        return collected_container(destination).map(|head| format!("collect::<{head}>"));
    }
    if let Some(self_type) = path::qualified_self(&plain) {
        let head = path::type_head(self_type);
        let allocates = match operation {
            "clone" => CLONE_ALLOCATES.contains(&head),
            "to_owned" => TO_OWNED_ALLOCATES.contains(&head),
            _ => return allocating_call(destination),
        };
        return allocates.then(|| format!("{head}::{operation}"));
    }
    allocating_call(destination)
}

/// The container a resolved `collect` was building, when it allocates.
///
/// The target is written into the destination, so collecting into a `Vec` and
/// collecting into `()` stop looking alike. A `Result<Vec<_>, _>` still builds
/// the vector, so the container is looked for anywhere in the target rather
/// than only at its head.
fn collected_container(destination: &str) -> Option<&'static str> {
    let target = destination.rsplit_once("::<")?.1;
    COLLECT_ALLOCATES
        .iter()
        .find(|head| path::mentions_type(target, head))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_allocation_however_the_callee_is_spelled() {
        for callee in [
            "Vec::with_capacity",
            "Vec::<u8>::with_capacity",
            "alloc::vec::Vec::<T>::with_capacity",
            "std::vec::Vec::with_capacity",
        ] {
            assert_eq!(
                allocating_call(callee).as_deref(),
                Some("Vec::with_capacity"),
                "{callee}"
            );
        }
        assert_eq!(allocating_call("Box::new").as_deref(), Some("Box::new"));
        assert_eq!(
            allocating_call("values.collect::<Vec<_>>").as_deref(),
            Some("collect")
        );
        assert_eq!(
            allocating_call("self.name.to_string").as_deref(),
            Some("to_string")
        );
    }

    /// An empty container touches no allocator, and syntax cannot type a
    /// receiver. Both are the difference between a usable rule and a noisy one.
    #[test]
    fn declines_what_it_cannot_establish() {
        for callee in [
            "Vec::new",
            "String::new",
            "HashMap::new",
            "BTreeMap::new",
            "handle.clone",
            "value.to_owned",
            "value.into",
            "Foo::with_capacity",
        ] {
            assert_eq!(allocating_call(callee), None, "{callee}");
        }
    }

    #[test]
    fn an_empty_macro_expansion_allocates_nothing() {
        assert_eq!(allocating_macro("format", true), Some("format!"));
        assert_eq!(allocating_macro("vec", true), Some("vec!"));
        assert_eq!(allocating_macro("vec", false), None);
        assert_eq!(allocating_macro("println", true), None);
    }
}
