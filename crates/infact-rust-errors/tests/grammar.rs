#![allow(clippy::unwrap_used, clippy::expect_used)]
//! What the analyzer recognizes, and what it deliberately leaves alone.

use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use infact_core::{Certainty, Containment, DiscardForm, ErrorDiscard, Reach};

fn parser_packs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust")
}

fn discards(source: &str) -> Vec<ErrorDiscard> {
    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let parsed = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser")
        .parse("source.rs", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source");
    infact_rust_errors::analyze_file(&parsed).expect("analyzing source")
}

fn forms(source: &str) -> Vec<DiscardForm> {
    discards(source)
        .into_iter()
        .map(|found| found.form)
        .collect()
}

#[test]
fn recognizes_each_discarding_form() {
    assert_eq!(
        forms("fn a() { let _ = write(); }"),
        [DiscardForm::LetUnderscore]
    );
    assert_eq!(
        forms("fn a() -> Option<u32> { thing().ok() }"),
        [DiscardForm::OkDiscard]
    );
    assert_eq!(
        forms("fn a() { let x = thing().unwrap_or_default(); }"),
        [DiscardForm::UnwrapOr]
    );
    assert_eq!(
        forms("fn a() { match thing() { Ok(v) => v, Err(_) => return } }"),
        [DiscardForm::ErrArm]
    );
    assert_eq!(
        forms("fn a() { if let Ok(v) = thing() { use_it(v) } }"),
        [DiscardForm::OkBinding]
    );
    assert_eq!(
        forms("fn a() { let Ok(v) = thing() else { return }; }"),
        [DiscardForm::OkBinding]
    );
    assert_eq!(
        forms("fn a() { items.filter_map(Result::ok).count(); }"),
        [DiscardForm::IteratorDrop]
    );
    assert_eq!(
        forms("fn a() -> Result<u32> { thing().map_err(|_| Other)? }"),
        [DiscardForm::CauseErased]
    );
    assert_eq!(forms("fn a() { thing().unwrap(); }"), [DiscardForm::Panic]);
}

/// The forms that keep the cause are not discards, and must not be reported.
#[test]
fn leaves_handled_errors_alone() {
    assert!(forms("fn a() -> Result<u32> { thing()? }").is_empty());
    assert!(forms("fn a() { match thing() { Ok(v) => v, Err(e) => report(e) } }").is_empty());
    assert!(forms("fn a() -> Result<u32> { thing().map_err(|e| Wrap(e)) }").is_empty());
    assert!(forms("fn a() { thing().unwrap_or_else(|e| fallback(e)); }").is_empty());
    assert!(forms("fn a() { let value = thing(); }").is_empty());
}

/// Some `Result`s answer a question rather than report a failure.
///
/// `binary_search` says "not present" with `Err(insertion_point)`, and
/// `Path::strip_prefix` says "not under this prefix". Discarding those
/// discards nothing, and reporting them would bury the real findings.
#[test]
fn leaves_query_results_alone() {
    assert!(forms("fn a() { items.binary_search_by_key(&id, key).ok(); }").is_empty());
    assert!(forms("fn a() { items.binary_search(&id).ok(); }").is_empty());
    assert!(
        forms("fn a() -> Option<PathBuf> { path.strip_prefix(root).ok().map(PathBuf::from) }")
            .is_empty()
    );
    assert!(
        forms("fn a() { if let Ok(rest) = path.strip_prefix(root) { use_it(rest) } }").is_empty()
    );

    // the same shapes on a genuinely fallible call are still reported
    assert_eq!(
        forms("fn a() { std::fs::read(path).ok(); }"),
        [DiscardForm::OkDiscard]
    );
}

/// `.ok()` names `Result`; `.unwrap_or_default()` reads the same on `Option`.
#[test]
fn separates_certain_forms_from_ambiguous_ones() {
    let certain = discards("fn a() { thing().ok(); }");
    assert_eq!(certain[0].certainty, Certainty::Certain);
    let possible = discards("fn a() { thing().unwrap_or_default(); }");
    assert_eq!(possible[0].certainty, Certainty::Possible);

    // `let _ =` names no type, but exists only to drop a must-use value.
    let discarded = discards("fn a() { let _ = write(); }");
    assert_eq!(discarded[0].certainty, Certainty::Certain);
}

/// The signal that an error had nowhere to go: the callable cannot return one.
#[test]
fn records_whether_the_callable_could_have_propagated() {
    let infallible = discards("fn a() { let _ = write(); }");
    assert_eq!(infallible[0].containment, Containment::Infallible);

    let fallible = discards("fn a() -> Result<()> { let _ = write(); Ok(()) }");
    assert_eq!(fallible[0].containment, Containment::Fallible);

    let optional = discards("fn a() -> Option<u32> { let _ = write(); None }");
    assert_eq!(optional[0].containment, Containment::Optional);

    let aliased = discards("fn a() -> anyhow::Result<()> { let _ = write(); Ok(()) }");
    assert_eq!(aliased[0].containment, Containment::Fallible);
}

#[test]
fn attributes_a_discard_to_its_enclosing_callable() {
    let found = discards("impl Reader { fn read(&self) { let _ = write(); } }");
    assert_eq!(found[0].callable, "source::Reader::read");
    assert_eq!(found[0].span.start_line, 1);
}

/// A policy usually exempts tests, so the fact has to say which sites are tests.
#[test]
fn marks_test_sites() {
    let found = discards("#[cfg(test)]\nmod tests {\n#[test]\nfn a() { thing().unwrap(); }\n}");
    assert!(found.iter().all(|found| found.in_test));
    assert!(!discards("fn a() { thing().unwrap(); }")[0].in_test);
}

/// The question the local signature cannot answer: could ANY caller be told?
#[test]
fn resolves_how_far_a_failure_could_have_travelled() {
    // the discarding callable returns Result, so it could have returned this
    let local = discards("fn a() -> Result<()> { let _ = write(); Ok(()) }");
    assert_eq!(local[0].reach, Reach::Local);

    // infallible, but a caller above returns Result and could have been told
    let ancestor = discards(
        "fn top() -> Result<()> { middle(); Ok(()) }\n\
         fn middle() { leaf(); }\n\
         fn leaf() { let _ = write(); }\n",
    );
    assert_eq!(ancestor[0].reach, Reach::Ancestor);
    let steps = ancestor[0]
        .path
        .iter()
        .map(|edge| format!("{}->{}", edge.caller, edge.callee))
        .collect::<Vec<_>>();
    assert_eq!(
        steps,
        [
            "source::top->source::middle",
            "source::middle->source::leaf"
        ]
    );

    // every caller is infallible, so nothing in this chain can report it
    let sealed = discards(
        "fn top() { middle(); }\n\
         fn middle() { leaf(); }\n\
         fn leaf() { let _ = write(); }\n",
    );
    assert_eq!(sealed[0].reach, Reach::Sealed);
    assert_eq!(sealed[0].path.len(), 2);

    // no caller could be resolved, so the answer is not known rather than sealed
    let unknown = discards("fn orphan() { let _ = write(); }");
    assert_eq!(unknown[0].reach, Reach::Unknown);
    assert!(unknown[0].path.is_empty());
}

/// A cycle in the call graph must not hang the search.
#[test]
fn survives_recursive_callers() {
    let found = discards(
        "fn a() { b(); leaf(); }\n\
         fn b() { a(); }\n\
         fn leaf() { let _ = write(); }\n",
    );
    assert_eq!(found[0].reach, Reach::Sealed);
}

/// An ambiguous name resolves to no edge, so reach must not be overstated.
#[test]
fn declines_to_resolve_an_ambiguous_callee() {
    let found = discards(
        "fn top() -> Result<()> { leaf(); Ok(()) }\n\
         mod other { fn leaf() {} }\n\
         fn leaf() { let _ = write(); }\n",
    );
    assert_eq!(found[0].reach, Reach::Unknown);
}

/// A sealed chain names the furthest caller, not the longest walk to one.
///
/// `c` calls `leaf` directly and also reaches it the long way round through
/// `b` and `a`. A search that reports whichever chain it happened to build
/// last answers three; the furthest caller from `leaf` is `b`, two calls up,
/// because `c` is only ever one call away.
#[test]
fn a_sealed_chain_is_measured_from_the_shortest_route() {
    let found = discards(
        "fn c() { b(); leaf(); }\n\
         fn b() { a(); }\n\
         fn a() { leaf(); }\n\
         fn leaf() { let _ = write(); }\n",
    );
    assert_eq!(found[0].reach, Reach::Sealed);
    let steps = found[0]
        .path
        .iter()
        .map(|edge| format!("{}->{}", edge.caller, edge.callee))
        .collect::<Vec<_>>();
    assert_eq!(steps, ["source::b->source::a", "source::a->source::leaf"]);
}
