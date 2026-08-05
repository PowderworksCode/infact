#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The contract: what is lifted prints back as the same program.
//!
//! Every case below is a bug that reached a compiler. They are kept because
//! each one printed something that *looked* right — a dropped `_` makes
//! `Some(_)` into `Some()`, a dropped `mut` makes a valid program that no
//! longer borrows — and none was visible without building the result.

use std::path::PathBuf;
use std::sync::Arc;

use entl_tree_sitter::{ParsedFile, ParserPack, ParserRuntime};
use infact_rust_lower::{lift_file, print, reprint_file};

fn parse(source: &str) -> ParsedFile {
    let pack = Arc::new(
        ParserPack::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust"),
        )
        .expect("rust parser pack"),
    );
    ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser")
        .parse("source.rs", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source")
}

/// The body of the one function in `source`, printed back out.
fn printed(source: &str) -> String {
    let parsed = parse(source);
    let bodies = lift_file(&parsed);
    assert_eq!(bodies.len(), 1, "expected exactly one function");
    print::block(&bodies[0].block, 0)
}

/// Lifting what was printed gives the same tree, which is what says the
/// printer and the lift agree rather than merely both running.
fn stable(source: &str) {
    let parsed = parse(source);
    let once = lift_file(&parsed);
    let reprinted = reprint_file(&parsed.source, &once);
    let again = parse(&reprinted);
    let twice = lift_file(&again);
    assert_eq!(
        once.len(),
        twice.len(),
        "same number of bodies\n{reprinted}"
    );
    for (before, after) in once.iter().zip(twice.iter()) {
        assert_eq!(
            before.block, after.block,
            "lifting the printed form gives the same tree\n{reprinted}"
        );
    }
}

/// `_` is anonymous in the grammar, so asking for the named children drops it
/// and `Some(_)` prints as `Some()`. Eleven of these were in one crate.
#[test]
fn a_discarded_position_is_still_a_position() {
    let source = "fn f(v: Option<u32>) -> u32 { match v { Some(_) => 1, None => 0 } }";
    assert!(
        printed(source).contains("Some(_)"),
        "got: {}",
        printed(source)
    );
    stable(source);
}

/// `let mut x` puts the `mut` beside the pattern rather than around it, so a
/// binding does not carry it. Dropping it compiles until something borrows.
#[test]
fn a_mutable_binding_stays_mutable() {
    let source = "fn f() { let mut total = 0; total += 1; }";
    assert!(
        printed(source).contains("let mut total"),
        "{}",
        printed(source)
    );
    stable(source);
}

/// A closure's discarded parameter is anonymous too, and losing it makes the
/// closure take no arguments at all.
#[test]
fn a_closure_that_discards_its_argument_still_takes_one() {
    let source =
        r#"fn f(r: Result<u32, u32>) -> Result<u32, String> { r.map_err(|_| "no".to_owned()) }"#;
    assert!(printed(source).contains("|_|"), "{}", printed(source));
    stable(source);
}

/// The grammar's `return_type` field is the type without its arrow, so a
/// closure printed from it reads as a chain of comparisons.
#[test]
fn a_closure_return_type_keeps_its_arrow() {
    let source = "fn f() { let g = |x: u32| -> u32 { x + 1 }; }";
    assert!(printed(source).contains("-> u32"), "{}", printed(source));
    stable(source);
}

/// A comment that followed code stays on that line. `straitjacket-allow` is
/// scoped to its line, so moving it down changes which statement it covers.
#[test]
fn a_trailing_comment_stays_on_its_line() {
    let source = "fn f() -> u32 {\n    1 // straitjacket-allow:something\n}";
    let output = printed(source);
    assert!(
        output.contains("1 // straitjacket-allow:something"),
        "{output}"
    );
    stable(source);
}

/// A comment on its own line stays on its own line.
#[test]
fn a_standalone_comment_is_not_pulled_up() {
    let source = "fn f() -> u32 {\n    // why\n    1\n}";
    let output = printed(source);
    assert!(output.contains("// why\n"), "{output}");
    assert!(!output.contains("// why 1"), "{output}");
}

/// Whether a block's last statement had a semicolon decides whether the block
/// has a value.
#[test]
fn a_missing_semicolon_is_the_blocks_value() {
    assert!(printed("fn f() -> u32 { 1 }").contains('1'));
    assert!(printed("fn f() { g(); }").contains("g();"));
    stable("fn f() -> u32 { let a = 1; a }");
}

/// `&x`, `&mut x` and `x` are three different programs. The `Form` lift makes
/// them one, which is what `notes/LOWERING.md` measures; this one does not.
#[test]
fn a_borrow_is_not_noise() {
    assert!(printed("fn f(v: &Vec<u32>) -> &[u32] { &v[..] }").contains('&'));
    let mutable = printed("fn f(v: &mut Vec<u32>) { g(&mut v); }");
    assert!(mutable.contains("&mut v"), "{mutable}");
}

/// A one-element tuple needs its comma, or it is a parenthesized expression.
#[test]
fn a_one_element_tuple_keeps_its_comma() {
    assert!(printed("fn f() -> (u32,) { (1,) }").contains("(1,)"));
}

/// Everything outside a body is copied byte for byte, so a signature, an
/// import or a type declaration is never at risk from a printing bug.
#[test]
fn nothing_outside_a_body_is_touched() {
    let source = "use std::fmt;\n\n#[derive(Debug)]\npub struct Point<T: Copy> {\n    pub x: T,\n}\n\npub fn f<'a, T>(value: &'a T) -> &'a T\nwhere\n    T: Copy,\n{\n    value\n}\n";
    let parsed = parse(source);
    let printed = reprint_file(&parsed.source, &lift_file(&parsed));
    for line in [
        "use std::fmt;",
        "#[derive(Debug)]",
        "pub struct Point<T: Copy> {",
        "pub fn f<'a, T>(value: &'a T) -> &'a T",
        "where",
        "    T: Copy,",
    ] {
        assert!(printed.contains(line), "{line} was lost:\n{printed}");
    }
}

/// A macro's tokens are not necessarily an expression — `matches!` takes a
/// pattern — so they are held exactly and printed back unchanged.
#[test]
fn a_macro_keeps_its_tokens_exactly() {
    let source = r#"fn f(v: Option<u32>) -> bool { matches!(v, Some(n) if n > 2) }"#;
    assert!(
        printed(source).contains("matches!(v, Some(n) if n > 2)"),
        "{}",
        printed(source)
    );
    stable(source);
}

/// The shapes that made the `Form` round trip fail, together.
#[test]
fn the_cases_that_defeated_lowering_from_a_normalized_form() {
    let source = r"
fn f(values: Vec<String>) -> usize {
    let mut counts = std::collections::HashMap::with_capacity(8);
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    counts.len()
}";
    let output = printed(source);
    // the constructor and its argument, which `Construct` discards
    assert!(output.contains("with_capacity(8)"), "{output}");
    // the mutability, which `Let` discards
    assert!(output.contains("let mut counts"), "{output}");
    // the dereference, which `unwrap_noise` strips
    assert!(output.contains("*counts"), "{output}");
    stable(source);
}

/// An or-pattern binds names, and the `Form` lift turns it into a hole. Here
/// the alternatives and their bindings survive.
#[test]
fn an_or_pattern_keeps_its_alternatives() {
    let source = "fn f(v: Result<u32, u32>) -> u32 { match v { Ok(n) | Err(n) => n } }";
    assert!(
        printed(source).contains("Ok(n) | Err(n)"),
        "{}",
        printed(source)
    );
    stable(source);
}
