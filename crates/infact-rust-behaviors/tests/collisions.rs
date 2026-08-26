#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Distinct callables must derive distinct behaviors.
//!
//! When two callables reduce to one form, every match against that form names
//! one of them and is wrong about the other — silently, because a finding
//! carries no sign that it was ambiguous. Some collisions are genuine: `map` on
//! `Option` and on `Result` really are the same behavior, and code that
//! reimplements one has reimplemented both. The rest are distinctions the
//! normalizer erased, and those are bugs.
//!
//! Four such erasures were found by accident before this test existed — `true`
//! reducing to `()`, `filter_map` to `map`, `next_back` outranking `next`, and
//! `e.into()` to `e` — each producing hundreds to thousands of wrong findings.
//! Two more were found the moment it did exist: `Self` read as a type name, and
//! a discarded tuple position dropped rather than held, which made
//! `BTreeMap::keys` and `BTreeMap::values` the same behavior.
//!
//! Type information will resolve *some* collisions — `HashMap::Entry::key` and
//! `BTreeMap::Entry::key` are one behavior on two types, and knowing the
//! receiver picks the right one. It will resolve none where the candidates share
//! a receiver: `Result::into_ok` and `Result::into_err` are both on `Result`, so
//! no amount of type information separates them, and a resolver that returned
//! one would be guessing while looking authoritative. That is the distinction
//! this test draws, so that adding types narrows what it should and hides
//! nothing.

use std::collections::BTreeMap;
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

/// The last segment of a callable path, which is the name a reader uses.
fn leaf(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// Whether two names describe the same operation with a different knob.
///
/// `counts` and `counts_with_hasher` differ only in what the caller supplies,
/// so deriving one form is correct and the plainer name is already preferred
/// when reporting. A longer name that extends a shorter one is the general
/// shape of that. It is a heuristic, and it would hide a real erasure between
/// `unwrap_or` and `unwrap_or_else` — worth revisiting if that ever collides.
fn one_extends_the_other(left: &str, right: &str) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

/// The type a callable belongs to, which is what type information can tell apart.
fn container(path: &str) -> &str {
    path.rsplit_once("::")
        .map_or("", |(container, _)| container)
}

/// No two callables *on the same type* may share a form.
///
/// Sharing across different types is left to type information, which is exactly
/// the question it can answer. Sharing within one type is a distinction the
/// normalizer erased, and nothing downstream can recover it.
#[test]
fn distinct_callables_derive_distinct_behaviors() {
    let library = derive_library(
        crate_root().join("tests/fixtures/itertools-source"),
        &parsers(),
        "itertools",
        "0.15.0",
    )
    .unwrap();

    let mut by_form: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for behavior in &library.behaviors {
        by_form
            .entry(behavior.program.to_string())
            .or_default()
            .push(&behavior.callable_path);
    }

    let erased = by_form
        .iter()
        .filter(|(_, paths)| {
            let names = paths
                .iter()
                .map(|path| leaf(path))
                .collect::<std::collections::BTreeSet<_>>();
            let _ = &names;
            paths.iter().any(|left| {
                paths.iter().any(|right| {
                    leaf(left) != leaf(right)
                        && !one_extends_the_other(leaf(left), leaf(right))
                        // a difference in type is a difference types can settle
                        && container(left) == container(right)
                })
            })
        })
        .map(|(form, paths)| format!("{paths:?}\n    {form}"))
        .collect::<Vec<_>>();

    assert!(
        erased.is_empty(),
        "{} forms are shared by differently-named callables on the same type. \
         No type information can separate these, so a match against them names \
         the wrong API and always will:\n  {}",
        erased.len(),
        erased.join("\n  ")
    );
}

/// A behavior another one is broader than stands aside where that one landed.
///
/// `Option::and_then` is `match self { Some(x) => f(x), None => None }`, and the
/// hole swallows what every narrower way of consuming an `Option` puts there.
/// Measured on clippy's `manual_map` test it landed on fifteen of the same lines
/// `Option::map` did, saying less about each.
#[test]
fn a_broader_behavior_stands_aside_where_a_narrower_one_landed() {
    use infact_core::Form;
    use infact_normalize::{Arm, Pattern};

    let hole_applied = || Form::Call {
        callee: Box::new(Form::Free(1)),
        arguments: vec![Form::Local(0)],
    };
    let none = || Form::Variant {
        name: "None".to_owned(),
        payload: Vec::new(),
    };
    let consuming = |taken: Form| {
        Form::select(
            Form::Free(0),
            vec![
                Arm {
                    pattern: Pattern::Variant {
                        name: "Some".to_owned(),
                        parts: vec![Pattern::Binding(0)],
                    },
                    body: taken,
                },
                Arm {
                    pattern: Pattern::Variant {
                        name: "None".to_owned(),
                        parts: Vec::new(),
                    },
                    body: none(),
                },
            ],
        )
    };
    // `and_then` hands back whatever the caller's function returned; `map`
    // wraps it. The first accepts the second and not the other way about.
    let and_then = consuming(hole_applied());
    let map = consuming(Form::Variant {
        name: "Some".to_owned(),
        payload: vec![hole_applied()],
    });
    assert!(
        map.contains(&and_then),
        "and_then must accept map, or it is not the broader of the two"
    );
    assert!(
        !and_then.contains(&map),
        "map must not accept and_then, or neither is broader"
    );
}
