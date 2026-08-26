#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A constructed type is resolved where it is written, not by bare name.
//!
//! `container` on a library function is an unqualified type name, so a candidate
//! set built from the name alone pools every type in the library that answers to
//! it. The standard library has four distinct types named `Cursor` — in
//! `linked_list`, `btree::set`, `btree::map` and `io` — and every one of them
//! resolved to a single `next`. `LinkedList::cursor_back` was handed a form
//! reading `self.inner.next()`, and `linked_list::Cursor` has no `inner` field:
//! the behavior belonged to another type outright. Twenty-one std callables were
//! attributed a foreign type's behavior that way, and nothing downstream carried
//! any sign of it.
//!
//! Both directions matter. Refusing to follow anything would also make this pass
//! while destroying the feature, so the same-file case is asserted alongside.

use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_rust_behaviors::derive_library;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

fn derived() -> Vec<(String, String)> {
    let library = derive_library(
        crate_root().join("tests/fixtures/same-name-types"),
        &parsers(),
        "samename",
        "0.0.0",
    )
    .unwrap();
    library
        .behaviors
        .iter()
        .map(|behavior| (behavior.callable_path.clone(), behavior.program.to_string()))
        .collect()
}

fn form_of<'a>(behaviors: &'a [(String, String)], name: &str) -> Option<&'a str> {
    behaviors
        .iter()
        .find(|(path, _)| path.rsplit("::").next() == Some(name))
        .map(|(_, form)| form.as_str())
}

/// A type's behavior may only come from the file that declares it.
#[test]
fn a_constructor_is_not_given_a_same_named_foreign_types_behavior() {
    let behaviors = derived();

    for name in ["cursor_front", "cursor_back"] {
        if let Some(form) = form_of(&behaviors, name) {
            assert!(
                !form.contains("inner"),
                "{name} constructs the `Cursor` in list.rs, which has no `inner` \
                 field. Reading one means it was given pairs.rs's `Cursor::next`:\n  {form}"
            );
        }
    }
}

/// The other direction: scoping must not simply stop derivation from following.
#[test]
fn a_constructor_still_reaches_its_own_types_method() {
    let behaviors = derived();
    let form = form_of(&behaviors, "keys").expect(
        "`keys` constructs the `Cursor` declared beside it, whose `next` is the \
         behavior it stands for — refusing this would disable adaptor following",
    );
    assert!(
        form.contains("inner"),
        "`keys` should carry its own Cursor::next, which reads `self.inner`:\n  {form}"
    );
}

/// A helper nested in a method body does not answer to the surrounding type.
///
/// Carried down, the container made a local `fn total` a second callable named
/// `Walk::total`, and the resolver refuses a name two callables answer to — so
/// the real `total`, with a perfectly good body, reported that no
/// implementation was found.
///
/// `Iterator::fold` is the case this was found on: `max_by` and `min_by`
/// declare their own `fn fold` helpers, and three callables claimed the name.
#[test]
fn a_helper_nested_in_a_method_does_not_shadow_its_sibling() {
    let crate_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parsers =
        entl_tree_sitter::ParserCatalog::discover([crate_root.join("../../../entl/parser-packs")]);
    assert!(parsers.errors.is_empty(), "{:?}", parsers.errors);

    let derived = infact_rust_behaviors::derive_library(
        crate_root.join("tests/fixtures/nested-helper"),
        &parsers.catalog,
        "probe",
        "0.1.0",
    )
    .expect("deriving the fixture");

    let paths: std::collections::BTreeSet<&str> = derived
        .behaviors
        .iter()
        .map(|behavior| behavior.callable_path.as_str())
        .collect();
    assert!(
        paths.contains("probe::Walk::total"),
        "the method the nested helper shares a name with must still resolve: {paths:?}"
    );
}
