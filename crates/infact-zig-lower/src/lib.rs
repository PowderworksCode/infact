//! A faithful Zig syntax tree, and what of it translates to Rust.
//!
//! The Rust side of this is `infact-rust-lower`, which lifts Rust and prints it
//! back. That crate can lean on `Expr::Verbatim` for anything it does not
//! understand, because Rust text held as text prints back as valid Rust.
//!
//! **No such escape exists here, and it is the central difference.** Zig text
//! is not Rust, so a translator cannot decline by copying. It must translate a
//! construct correctly or refuse it out loud. `baozi/PORTING.md` already
//! specifies what refusal looks like — `todo!("port: <why>")` — so a mechanical
//! translator fits the existing pipeline by filling what it is certain of and
//! leaving the rest marked for a model.
//!
//! That makes the number to maximize *coverage with zero wrong answers*, rather
//! than the round-trip fidelity the Rust crate measures.

// The module doc quotes the porting protocol's refusal spelling, which is the
// literal text a translator emits — the thing being specified, not work this
// crate deferred. File-scoped because the rule matches the quoted spelling.
// straitjacket-allow-file:stray-todo

/// Rewrite Bun's `#private` field syntax so the grammar can read it.
///
/// `tree-sitter-zig` rejects `#` in an identifier. Bun uses it for private
/// struct members, and an `ERROR` node swallows every declaration inside its
/// envelope, so a file using one contributes a partial plan without saying so.
///
/// Measured over Bun v1.3.14: **2,092 occurrences across 140 files**, which is
/// 90% of all parse damage in the corpus. `baozi/parse-patches.json` patches a
/// single site and records the belief that "the other six sites in the tree are
/// nested and were never indexed"; that is an undercount by two orders of
/// magnitude, and the note is what this function exists to correct.
///
/// The rewrite is **length-preserving**, so byte offsets, line numbers and
/// column positions are unchanged and anything keyed on `file:line` still
/// lines up. `#` becomes `_`, which is one character for one and leaves the
/// name recoverable.
///
/// Only a `#` that begins an identifier **in code** is touched. A markdown
/// heading in a test fixture is `# Heading`, with a space; and `"#foo"` inside
/// a string is data, not a field, so rewriting it would corrupt the program it
/// is trying to read. Bun ships markdown fixtures full of both.
///
/// Strings and comments are skipped by scanning each line: Zig's quoted strings
/// do not span lines, its multiline strings are `\\`-prefixed per line, and a
/// `//` comment runs to the end of one. That is enough to know what is code
/// without parsing, which matters because the parse is what this is repairing.
#[must_use]
pub fn patch_private_fields(source: &[u8]) -> Vec<u8> {
    let mut output = source.to_vec();
    let mut index = 0usize;
    let mut in_string = false;
    let mut in_char = false;
    let mut skip_line = false;
    while index < output.len() {
        let byte = output[index];
        match byte {
            b'\n' => {
                // Nothing carries across a line: not a string, not a comment.
                in_string = false;
                in_char = false;
                skip_line = false;
            }
            _ if skip_line => {}
            b'\\' if in_string || in_char => {
                // an escape consumes the next byte, so `"\""` stays open
                index += 1;
            }
            // `\\` begins a multiline string, whose whole line is content
            b'\\' if output.get(index + 1) == Some(&b'\\') => skip_line = true,
            // `//` begins a comment, and a `#` after it is prose
            b'/' if !in_string && !in_char && output.get(index + 1) == Some(&b'/') => {
                skip_line = true;
            }
            b'"' if !in_char => in_string = !in_string,
            b'\'' if !in_string => in_char = !in_char,
            b'#' if !in_string && !in_char => {
                let starts_name = output
                    .get(index + 1)
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_');
                // and must not continue one, so `a#b` is left alone
                let continues_name = index > 0
                    && (output[index - 1].is_ascii_alphanumeric() || output[index - 1] == b'_');
                if starts_name && !continues_name {
                    output[index] = b'_';
                }
            }
            _ => {}
        }
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::patch_private_fields;

    fn patched(source: &str) -> String {
        String::from_utf8(patch_private_fields(source.as_bytes())).expect("utf-8")
    }

    #[test]
    fn a_private_field_becomes_readable() {
        assert_eq!(patched("#socket: Socket = .{},"), "_socket: Socket = .{},");
        assert_eq!(patched(".#pointer = data,"), "._pointer = data,");
    }

    /// Offsets must not move, or every `file:line` keyed on the original
    /// stops lining up.
    #[test]
    fn the_rewrite_preserves_length() {
        let source = "#a\n#bb\nplain\n";
        assert_eq!(patched(source).len(), source.len());
        assert_eq!(patched(source).lines().count(), source.lines().count());
    }

    /// A markdown heading in a fixture is not a private field.
    #[test]
    fn a_hash_that_starts_no_name_is_left_alone() {
        assert_eq!(patched("x = 1; // # note"), "x = 1; // # note");
    }

    /// Bun ships markdown fixtures, so `#heading` inside a string is data.
    /// Rewriting it would corrupt the program this is trying to read.
    #[test]
    fn a_hash_inside_a_string_is_data() {
        assert_eq!(
            patched(r##"const s = "#heading";"##),
            r##"const s = "#heading";"##
        );
        assert_eq!(patched(r"    \\#heading text"), r"    \\#heading text");
        assert_eq!(patched("// see #note"), "// see #note");
    }

    /// An escaped quote does not end the string it is inside.
    #[test]
    fn an_escape_does_not_end_a_string() {
        assert_eq!(patched(r##"a("\"#x") ; #real"##), r##"a("\"#x") ; _real"##);
    }

    /// A field is still rewritten when a string sits on the same line.
    #[test]
    fn code_after_a_string_is_still_code() {
        assert_eq!(
            patched(r#"log("done"); self.#count = 1;"#),
            r#"log("done"); self._count = 1;"#
        );
    }
}
