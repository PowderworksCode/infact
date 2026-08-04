#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Distinct callables must normalize to distinct forms.
//!
//! When two callables reduce to one form, every match against that form names
//! one of them and is wrong about the other — silently, because a finding
//! carries no sign that it was ambiguous. `infact-rust-behaviors` has the same
//! test and it earned its place: four erasures were found by accident before it
//! existed, each producing hundreds to thousands of wrong findings, and two
//! more the moment it did.
//!
//! This is that test built alongside the normalizer rather than after it, which
//! is the point. It differs from the Rust one in what it can check: there is no
//! Python behavior derivation yet, so there is no library to derive from and no
//! receiver type to appeal to. What it has instead is a fixture of callables
//! chosen so that every pair is close enough for a plausible erasure to
//! collapse it, and each distinction is one a consumer would report on.
//!
//! Its limit is worth stating, because the corpus later demonstrated it: a
//! fixture only holds erasures someone thought of. The widest erasure in the
//! Python frontend — every called name resolving to a hole, so two calls to
//! different constructors were one form — was not in this file until the
//! `ambiguity` example measured it over the installed corpus and found 94.9%
//! of calls affected. It is here now, and the lesson is that this test and
//! that example answer different questions.
//!
//! It found one erasure the first time it ran, and it was the same erasure the
//! Rust frontend had already paid for once: a tuple literal reduced to a bare
//! `Construct("tuple")`, discarding the values it held, so `(a, b)` and
//! `(b, a)` were one form. The collision assertion did not catch it — the
//! fixture happened to hold only one such callable. The size floor below did,
//! by reporting a four-node form for a function that plainly says more than
//! four things. That is why both are here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime};
use infact_normalize::Form;
use infact_python_normalize::normalize_file;

fn parse(path: PathBuf) -> ParsedFile {
    let discovery = ParserCatalog::discover([
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs")
    ]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let pack = discovery
        .catalog
        .resolve("python", &path)
        .expect("no python parser pack")
        .clone();
    let source = std::fs::read(&path).expect("read fixture");
    let file = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("load parser")
        .parse(path, source)
        .expect("parse");
    assert!(!file.tree.root_node().has_error(), "the fixture must parse");
    file
}

fn fixture() -> Vec<(String, Form)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/distinct.py");
    normalize_file(&parse(path))
        .into_iter()
        .map(|found| (found.name, found.form.simplify().canonical()))
        .collect()
}

#[test]
fn distinct_callables_normalize_to_distinct_forms() {
    let functions = fixture();
    assert!(
        functions.len() >= 20,
        "the fixture shrank to {} callables",
        functions.len()
    );

    let mut by_form: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (name, form) in &functions {
        by_form
            .entry(form.to_string())
            .or_default()
            .push(name.as_str());
    }

    let collided: Vec<_> = by_form
        .iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(form, names)| format!("{}\n    {form}", names.join(", ")))
        .collect();
    assert!(
        collided.is_empty(),
        "these callables are different operations and share a form:\n  {}",
        collided.join("\n  ")
    );
}

/// A form small enough to collide across unrelated code is not worth reporting,
/// and the core says so with a floor of six nodes.
///
/// Some Python genuinely falls below it, and that is a finding rather than a
/// bug: `[k for k, v in pairs]` really is four nodes of behavior, and a
/// consumer reporting it would be reporting a shape rather than an API. What
/// this pins is that nothing ELSE drops below — a frontend that flattened a
/// multi-statement function into three nodes would be erasing, and would look
/// exactly like this until someone counted.
///
/// The list is a ratchet. A name joining it is a claim that the callable is a
/// one-line comprehension, and worth checking before it is added.
#[test]
fn only_the_one_line_comprehensions_fall_below_the_reportable_floor() {
    const EXPECTED_SMALL: &[&str] = &["keys_of", "values_of", "flattened", "mapped_lazily"];
    let below: Vec<_> = fixture()
        .into_iter()
        .filter(|(_, form)| form.size() < infact_normalize::MINIMUM_REPORTABLE_SIZE)
        .map(|(name, form)| format!("{name} ({}): {form}", form.size()))
        .collect();
    let unexpected: Vec<_> = below
        .iter()
        .filter(|entry| {
            !EXPECTED_SMALL
                .iter()
                .any(|name| entry.starts_with(&format!("{name} (")))
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "these dropped below the reportable floor and should not have:\n  {}",
        unexpected
            .iter()
            .map(|entry| entry.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    for name in EXPECTED_SMALL {
        assert!(
            below
                .iter()
                .any(|entry| entry.starts_with(&format!("{name} ("))),
            "{name} now clears the floor, so the list above is stale"
        );
    }
}

/// The specific erasures the other frontends have paid for, asked directly.
///
/// The collision test above would catch each of these, and would say only that
/// two names shared a form. These say which distinction was lost.
#[test]
fn the_distinctions_other_frontends_lost_are_held_here() {
    let by_name: BTreeMap<String, Form> = fixture().into_iter().collect();
    let form = |name: &str| {
        by_name
            .get(name)
            .unwrap_or_else(|| panic!("{name}"))
            .clone()
    };

    // Searching forwards and backwards. A reverse iterator outranking the
    // forward one is the mistake `infact-rust-normalize` made once.
    assert_ne!(form("first_matching"), form("last_matching"));
    // Which position of a destructured pair is kept. Dropping a discarded
    // tuple position made `BTreeMap::keys` and `BTreeMap::values` one behavior.
    assert_ne!(form("keys_of"), form("values_of"));
    // `filter_map` reducing to `map`: one can drop an element and one cannot.
    assert_ne!(form("mapped"), form("filtered"));
    // A non-numeric literal reducing to unit made `None => true` and an `if let`
    // with no else the same thing, 1,390 times over.
    assert_ne!(form("any_matching"), form("all_matching"));
    // Counting is not summing, however alike the loops look.
    assert_ne!(form("counted"), form("summed"));
    // The container being built is behavior; a set drops duplicates.
    assert_ne!(form("mapped"), form("mapped_set"));
    // Producing lazily is not gathering.
    assert_ne!(form("mapped"), form("mapped_lazily"));
    // Which failure is recovered from.
    assert_ne!(form("get_or_none"), form("parse_or_none"));
    // What is recovered WITH.
    assert_ne!(form("get_or_none"), form("get_or_default"));
    // A running total depends on the element before it; a map does not.
    assert_ne!(form("running_totals"), form("mapped"));
    // Stopping early is behavior.
    assert_ne!(form("take_while_positive"), form("filtered"));
    // Which class is being constructed. This pair is here because the CORPUS
    // reported it and this fixture did not contain it: `asyncio` writes six
    // pipe-transport factories differing in nothing but the constructor's
    // name, and while a called name resolved to a hole they were one form.
    assert_ne!(form("make_read_transport"), form("make_write_transport"));
}
