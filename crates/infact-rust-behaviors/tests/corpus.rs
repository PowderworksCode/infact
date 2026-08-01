#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A precision suite for library-opportunity findings.
//!
//! Recall is easy to claim and precision is what makes a finding worth reading,
//! so most of this corpus is code that must *not* match: behavior that looks
//! like a library API but differs, and bespoke types whose methods happen to
//! share names with the standard library's.
//!
//! Each fixture function carries its own expectation as a `// expect:` comment,
//! so the ground truth sits next to the code it describes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_core::{DerivedLibraryBehavior, ExternalCatalog};
use infact_rust_behaviors::analyze_repository;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus() -> PathBuf {
    crate_root().join("tests/fixtures/corpus")
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

fn packed() -> (Vec<ExternalCatalog>, Vec<DerivedLibraryBehavior>) {
    let packs = crate_root().join("../../infact-packs/rust-itertools");
    let catalog = serde_json::from_slice(
        &std::fs::read(packs.join("api/itertools-0.15.0.json")).expect("catalog"),
    )
    .expect("catalog parses");
    let behaviors = std::fs::read_dir(packs.join("behaviors"))
        .expect("behaviors")
        .filter_map(Result::ok)
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.path()).expect("behavior"))
                .expect("behavior parses")
        })
        .collect();
    (vec![catalog], behaviors)
}

/// How a fixture is expected to be reported.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expected {
    /// Reported as this API, as a direct reimplementation.
    Exact(String),
    /// Reported as this API, but done alongside other work.
    Fused(String),
    /// Not reported at all.
    None,
}

/// What each fixture function should be reported as, keyed by its file and the
/// line its definition starts on.
fn expectations() -> BTreeMap<(String, u32), Expected> {
    let mut expected = BTreeMap::new();
    let sources = std::fs::read_dir(corpus().join("src")).expect("corpus sources");
    for entry in sources.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(entry.path()).expect("fixture");
        let lines = text.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let (expectation, fused) = match (
                trimmed.strip_prefix("// expect: "),
                trimmed.strip_prefix("// expect-fused: "),
            ) {
                (Some(api), _) => (api, false),
                (_, Some(api)) => (api, true),
                _ => continue,
            };
            // the annotation describes the next definition below it
            let definition = lines
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, candidate)| candidate.starts_with("pub fn "))
                .map(|(line, _)| u32::try_from(line + 1).expect("line fits"))
                .unwrap_or_else(|| panic!("{name}: no definition follows an expectation"));
            let expectation = match (expectation.trim(), fused) {
                ("none", _) => Expected::None,
                (api, true) => Expected::Fused(api.to_owned()),
                (api, false) => Expected::Exact(api.to_owned()),
            };
            expected.insert((name.clone(), definition), expectation);
        }
    }
    assert!(!expected.is_empty(), "the corpus states no expectations");
    expected
}

/// Findings keyed by the fixture function they fall inside.
///
/// A finding points at the statements that carry the behavior, which is not
/// where the function starts, so each is attributed to the nearest definition
/// above it. Keying on exact lines would tie this suite to how precisely
/// findings happen to be located.
fn reported(expectations: &BTreeMap<(String, u32), Expected>) -> BTreeMap<(String, u32), Expected> {
    let (catalogs, behaviors) = packed();
    let report = analyze_repository(corpus(), &parsers(), &catalogs, &behaviors, &[]).unwrap();
    report
        .matches
        .iter()
        .filter_map(|fact| {
            let file = fact
                .value
                .span
                .path
                .file_name()
                .and_then(|name| name.to_str())?
                .to_owned();
            let enclosing = expectations
                .keys()
                .filter(|(candidate, line)| {
                    candidate == &file && *line <= fact.value.span.start_line
                })
                .map(|(_, line)| *line)
                .max()?;
            let api = fact.value.target.path().to_owned();
            Some((
                (file, enclosing),
                if fact.value.fused {
                    Expected::Fused(api)
                } else {
                    Expected::Exact(api)
                },
            ))
        })
        .collect()
}

/// A fused finding is a real but weaker claim, and must be told apart from a
/// direct reimplementation rather than folded in with it.
#[test]
fn work_done_alongside_a_behavior_is_reported_as_fused() {
    let expectations = expectations();
    let reported = reported(&expectations);
    let fused = expectations
        .iter()
        .filter(|(_, expectation)| matches!(expectation, Expected::Fused(_)))
        .collect::<Vec<_>>();
    assert!(!fused.is_empty(), "the corpus states no fused expectations");
    for (location, expectation) in fused {
        assert_eq!(reported.get(location), Some(expectation), "at {location:?}");
    }
}

/// Everything the corpus says is a reimplementation is found, as the right API.
#[test]
fn every_stated_reimplementation_is_reported() {
    let expectations = expectations();
    let reported = reported(&expectations);
    let mut missed = Vec::new();
    let mut misidentified = Vec::new();
    for (location, expectation) in expectations {
        if expectation == Expected::None {
            continue;
        }
        match reported.get(&location) {
            None => missed.push((location, expectation)),
            Some(actual) if actual != &expectation => {
                misidentified.push((location, expectation, actual.clone()));
            }
            Some(_) => {}
        }
    }
    assert!(missed.is_empty(), "not reported: {missed:?}");
    assert!(
        misidentified.is_empty(),
        "reported as the wrong API: {misidentified:?}"
    );
}

/// Nothing the corpus says is not a reimplementation is reported.
///
/// This is the half that decides whether the findings are worth reading.
#[test]
fn nothing_the_corpus_rules_out_is_reported() {
    let expectations = expectations();
    let reported = reported(&expectations);
    let spurious = expectations
        .clone()
        .into_iter()
        .filter(|(_, expectation)| expectation == &Expected::None)
        .filter_map(|(location, _)| {
            reported
                .get(&location)
                .map(|api| (location.clone(), api.clone()))
        })
        .collect::<Vec<_>>();
    assert!(spurious.is_empty(), "false positives: {spurious:?}");
}

/// A finding must come from a fixture the corpus has an opinion about, or the
/// suite is measuring less than it appears to.
#[test]
fn every_finding_is_accounted_for() {
    let expectations = expectations();
    let unexplained = reported(&expectations)
        .into_iter()
        .filter(|(location, _)| !expectations.contains_key(location))
        .collect::<Vec<_>>();
    assert!(
        unexplained.is_empty(),
        "unexplained findings: {unexplained:?}"
    );
}

/// The corpus has to be adversarial to be worth anything.
#[test]
fn the_corpus_is_mostly_negative() {
    let expectations = expectations();
    let negatives = expectations
        .values()
        .filter(|expectation| expectation == &&Expected::None)
        .count();
    assert!(
        negatives >= expectations.len() / 3,
        "only {negatives} of {} fixtures rule a match out",
        expectations.len()
    );
}
