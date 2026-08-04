#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Hand-written TypeScript is matched to the builtin it reimplements.
//!
//! This is the claim the whole crate exists to make, and until now only
//! `tools/ts-scoreboard` checked it — which is not part of the build, needs a
//! corpus fetched from two lint plugins, and cannot fail anybody's CI.
//!
//! Both sides are written the way their own author would write them. The
//! library is an index walk with the specification's coercions in it; the
//! repository is an index walk with none. Nothing here was written to make the
//! other side match.

use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_core::LibraryTarget;
use infact_ts_behaviors::{analyze_repository, derive_library};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

/// Every API named at each function of the fixture, by the line it starts on.
fn reported() -> Vec<(String, u32, u32)> {
    let parsers = parsers();
    let library = derive_library(
        crate_root().join("tests/fixtures/builtins-source"),
        &parsers,
        "ecmascript",
        "test",
    )
    .unwrap();
    let report = analyze_repository(
        crate_root().join("tests/fixtures/reimplements"),
        &parsers,
        std::slice::from_ref(&library.catalog),
        &library.behaviors,
    )
    .unwrap();
    assert!(
        report.diagnostics.is_empty(),
        "the fixture must be readable: {:?}",
        report.diagnostics
    );
    report
        .matches
        .iter()
        .filter_map(|matched| {
            let LibraryTarget::Callable { path, .. } = &matched.value.target else {
                return None;
            };
            Some((
                path.rsplit("::").next().unwrap_or(path).to_owned(),
                matched.value.span.start_line,
                matched.value.span.end_line,
            ))
        })
        .collect()
}

/// A forward search over an index walk is `find`, and a backward one is not.
///
/// The direction has to survive normalization or these two are one behavior.
/// It was measured going wrong exactly once, when direction was expressed as a
/// reversal wrapped around the sequence: the sequence is where a derived
/// behavior has a hole, a hole absorbs anything, and the forward search matched
/// the backwards code and named the opposite API.
#[test]
fn a_hand_written_search_is_matched_to_the_builtin_it_reimplements() {
    let reported = reported();
    let named = reported
        .iter()
        .map(|(api, _, _)| api.as_str())
        .collect::<Vec<_>>();
    assert!(
        named.contains(&"ArrayFind"),
        "the forward search was not recognized; got {reported:?}"
    );
    assert!(
        named.contains(&"ArrayFindLast"),
        "the backward search was not recognized; got {reported:?}"
    );
}

/// A match points at the statements that carry it, not at the file.
///
/// Naming the file is not much help in a four-hundred-line one, and a span that
/// covers everything is indistinguishable from a span nobody computed.
#[test]
fn a_match_is_placed_at_the_code_that_carries_it() {
    for (api, start, end) in reported() {
        assert!(
            start > 1 && end >= start,
            "{api} was reported at lines {start}-{end}, which is the whole file \
             rather than a place in it"
        );
        assert!(
            end - start < 20,
            "{api} spans {} lines, which is not a statement",
            end - start
        );
    }
}

/// A traversal that visits everything is not a search.
///
/// `countAdmins` has the same loop, the same index walk and the same condition
/// as `firstAdmin`; it differs only in that it does not stop. If `find` matched
/// it, `find` would match every loop with an `if` in it — which is what
/// `is_reportable` exists to prevent and what the counted-anchor floor was
/// calibrated against.
#[test]
fn a_walk_that_does_not_stop_is_not_reported_as_a_search() {
    let searches = reported()
        .into_iter()
        .filter(|(api, _, _)| api == "ArrayFind" || api == "ArrayFindLast")
        .collect::<Vec<_>>();
    // countAdmins is the third function in the file; the two searches are the
    // first two. A search reported anywhere past line 26 is on the counter.
    for (api, start, end) in searches {
        assert!(
            start < 27,
            "{api} was reported at lines {start}-{end}, which is the counting \
             loop rather than either search"
        );
    }
}

/// What the fixture reports, printed so a change in it is legible in the diff.
#[test]
fn the_fixture_reports_what_it_was_written_to_report() {
    let reported = reported();
    let rendered = reported
        .iter()
        .map(|(api, start, end)| format!("{api} {start}-{end}"))
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "ArrayFind 8-14".to_owned(),
            "ArrayFindLast 18-24".to_owned()
        ],
        "the two searches, each at the run of statements that carries it, and \
         nothing on the counting loop"
    );
}
