//! A Rust syntax tree that lifts from source and prints back to it.
//!
//! `notes/LOWERING.md` measures what happens when you try to emit code from
//! `infact_normalize::Form`: 4 of 120 functions survive the round trip, and
//! the reason is that every distinction an emitter needs is one normalization
//! exists to destroy. Lowering therefore does not belong on `Form`, and this
//! is where it lives instead.
//!
//! The two trees are for different jobs and are meant to coexist:
//!
//! | | `Form` | [`syntax`] |
//! |---|---|---|
//! | keeps | behavior | everything |
//! | identifiers | discarded | kept |
//! | `&x` vs `x` | one form | distinguished |
//! | `.iter()` | peeled as noise | kept |
//! | answers | are these the same? | what did this say? |
//!
//! Nothing here changes `Form` or how anything matches.

pub mod lift;
pub mod print;
pub mod syntax;

pub use lift::{Lifter, lift_file};
pub use syntax::{Block, Coverage, Expr, LiftedBody, Pat, Stmt};

/// Replace each lifted body in a source file with what printing it produced.
///
/// Everything outside a function body is copied byte for byte, so a signature,
/// an import or a type declaration is never at risk. What is measured is
/// exactly what was lifted.
#[must_use]
pub fn reprint_file(source: &[u8], bodies: &[LiftedBody]) -> String {
    let mut ordered = bodies.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|body| body.start_byte);
    let mut output = String::new();
    let mut cursor = 0usize;
    for body in ordered {
        let start = body.start_byte as usize;
        if start < cursor {
            continue;
        }
        output.push_str(&String::from_utf8_lossy(&source[cursor..start]));
        // The body is printed at the indentation of the line its signature
        // starts on, so the closing brace lands under the signature. Taking
        // the column of the opening brace instead indents by the width of the
        // signature, which is where `pub fn size(&self) -> u32 {` put a body
        // seven levels deep.
        let line_start = source[..start]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        let indent = source[line_start..start]
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        output.push_str(&print::block(&body.block, indent / 4));
        cursor = body.end_byte as usize;
    }
    output.push_str(&String::from_utf8_lossy(&source[cursor..]));
    output
}
