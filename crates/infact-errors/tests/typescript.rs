#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The same analyzer, a different language, and no Rust in between.
//!
//! Everything that makes these findings is data: `discards.scm` recognizes the
//! forms, `callables.scm` supplies the callables to attribute them to, and
//! `parser.toml` says how TypeScript spells failure. Nothing in
//! `infact-errors` names a TypeScript node kind, and nothing was added to it to
//! make this pass.

use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use infact_core::{Certainty, Containment, DiscardForm, ErrorDiscard, Reach};

fn typescript_pack() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/typescript")
}

fn discards(source: &str) -> Vec<ErrorDiscard> {
    let pack = Arc::new(ParserPack::load(typescript_pack()).expect("typescript parser pack"));
    let parser = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading typescript parser");
    let parsed = parser
        .parse("source.ts", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source");
    infact_errors::analyze_file(&parser, &parsed).expect("analyzing source")
}

fn forms(source: &str) -> Vec<DiscardForm> {
    discards(source).into_iter().map(|f| f.form).collect()
}

/// A `catch` that binds nothing is TypeScript's `Err(_)`.
#[test]
fn recognizes_a_handler_that_binds_nothing() {
    assert_eq!(
        forms("async function a() { try { await f(); } catch { } }"),
        [DiscardForm::ErrArm]
    );
}

/// A handler that binds the cause is not a discard, however little it does with
/// it — the same rule the Rust forms follow.
#[test]
fn leaves_a_bound_handler_alone() {
    assert!(
        forms("async function a() { try { await f(); } catch (error) { report(error); } }")
            .is_empty()
    );
}

/// `.catch(() => null)` turns a cause into an absence; `.catch((e) => ..)`
/// still sees it.
#[test]
fn separates_a_swallowed_rejection_from_a_handled_one() {
    assert_eq!(
        forms("async function a() { await f().catch(() => null); }"),
        [DiscardForm::OkDiscard]
    );
    assert!(forms("async function a() { await f().catch((e) => report(e)); }").is_empty());
}

/// `void f()` is the nearest thing TypeScript has to `let _ = f()`.
#[test]
fn recognizes_a_deliberately_discarded_call() {
    assert_eq!(
        forms("async function a() { void f(); }"),
        [DiscardForm::LetUnderscore]
    );
    assert!(forms("async function a() { f(); }").is_empty());
}

/// A discard is attributed to the callable holding it, and a method carries the
/// class it belongs to — the same `{module}::{container}::{name}` shape Rust
/// produces from an impl block.
#[test]
fn attributes_a_discard_to_its_class_and_method() {
    let found = discards("class Service { async load() { try { await f(); } catch { } } }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].callable, "source::Service::load");
}

/// Failure is unchecked in TypeScript, so every callable could have propagated
/// and every discard is reportable where it stands.
///
/// A synchronous function says nothing about failure in its signature, and it
/// still throws. Reading that as infallible would claim the error was trapped
/// when not catching it was available the whole time — so reach is `Local`, and
/// the search up the call graph never has to run.
#[test]
fn every_callable_could_have_propagated() {
    let found = discards("function sync(): string { try { f(); } catch { } return \"x\"; }");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].containment, Containment::Fallible);
    assert_eq!(found[0].reach, Reach::Local);
    assert_eq!(found[0].certainty, Certainty::Certain);
}
