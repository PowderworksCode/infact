//! The normalizations that let a library implementation meet its caller.
//!
//! The library implementations this crate exists to read are engine builtins
//! written in JavaScript, and they are MPL-2.0. Nothing is copied here: each
//! fixture is written to exercise one spelling those implementations use, so
//! the test states the rule rather than quoting a source.

use std::path::PathBuf;

use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime};
use infact_normalize::Form;
use infact_ts_normalize::normalize_file;

fn parsers() -> (ParserCatalog, ParserRuntime) {
    let discovery = ParserCatalog::discover([
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs")
    ]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    (
        discovery.catalog,
        ParserRuntime::new().expect("parser runtime"),
    )
}

fn parse(language: &str, name: &str, source: &str) -> ParsedFile {
    let file = parse_allowing_errors(language, name, source);
    assert!(!file.tree.root_node().has_error(), "{name} did not parse");
    file
}

fn parse_allowing_errors(language: &str, name: &str, source: &str) -> ParsedFile {
    let (catalog, runtime) = parsers();
    let path = PathBuf::from(name);
    let pack = catalog
        .resolve(language, &path)
        .unwrap_or_else(|| panic!("no {language} parser pack"))
        .clone();
    runtime
        .load(pack)
        .expect("load parser")
        .parse(path, source.as_bytes().to_vec())
        .expect("parse")
}

fn form(language: &str, name: &str, source: &str, function: &str) -> Form {
    normalize_file(&parse(language, name, source))
        .into_iter()
        .find(|found| found.name == function)
        .unwrap_or_else(|| panic!("{function} not found"))
        .form
        .simplify()
        .canonical()
}

fn javascript(source: &str, function: &str) -> Form {
    form("javascript", "library.js", source, function)
}

fn typescript(source: &str, function: &str) -> Form {
    form("typescript", "caller.ts", source, function)
}

/// The single most important rule: an index walk is a traversal.
///
/// JavaScript's canonical loop counts, and `for..of` does not. Both visit each
/// element, and if they normalized differently nothing else here would matter.
#[test]
fn an_index_walk_and_a_for_of_are_the_same_traversal() {
    let indexed = typescript(
        "export function f(xs: number[], t: number) {
           for (var k = 0; k < xs.length; k++) { var v = xs[k]; if (v === t) { return v; } }
           return undefined;
         }",
        "f",
    );
    let iterated = typescript(
        "export function f(xs: number[], t: number) {
           for (const v of xs) { if (v === t) { return v; } }
           return undefined;
         }",
        "f",
    );
    assert_eq!(
        indexed, iterated,
        "counting and iterating describe one walk"
    );
}

/// A walk whose body never names the element still binds one.
#[test]
fn an_element_accessed_in_place_becomes_the_item() {
    let unnamed = javascript(
        "function f(xs, p) {
           for (var k = 0; k < xs.length; k++) { if (p(xs[k])) { return true; } }
           return false;
         }",
        "f",
    );
    let named = javascript(
        "function f(xs, p) {
           for (var k = 0; k < xs.length; k++) { var v = xs[k]; if (p(v)) { return true; } }
           return false;
         }",
        "f",
    );
    assert_eq!(unnamed, named, "{unnamed}");
}

/// The specification's coercions return what they were given.
#[test]
fn spec_ceremony_carries_no_behavior() {
    let ceremonial = javascript(
        "function f(p) {
           var O = ToObject(this);
           var len = ToLength(O.length);
           if (!IsCallable(p)) { ThrowTypeError(1, 2); }
           for (var k = 0; k < len; k++) { if (callContentFunction(p, undefined, O[k], k, O)) { return O[k]; } }
           return undefined;
         }",
        "f",
    );
    let plain = javascript(
        "function f(p) {
           for (const v of this) { if (p(v)) { return v; } }
           return undefined;
         }",
        "f",
    );
    assert_eq!(
        ceremonial, plain,
        "coercion, precondition and call convention are not behavior:\n  {ceremonial}\n  {plain}"
    );
}

/// A library behavior is a pattern its caller fills in.
#[test]
fn a_hand_written_loop_matches_the_builtin_it_reimplements() {
    let library = javascript(
        "function ArrayFind(predicate) {
           var O = ToObject(this);
           var len = ToLength(O.length);
           if (!IsCallable(predicate)) { ThrowTypeError(1, 2); }
           for (var k = 0; k < len; k++) {
             var kValue = O[k];
             if (callContentFunction(predicate, undefined, kValue, k, O)) { return kValue; }
           }
           return undefined;
         }",
        "ArrayFind",
    );
    let caller = typescript(
        "export function findIt(xs: string[], t: string): string | undefined {
           for (const x of xs) { if (x === t) { return x; } }
           return undefined;
         }",
        "findIt",
    );
    assert!(
        caller.matches(&library),
        "the caller reimplements find:\n  library {library}\n  caller  {caller}"
    );
}

/// Code that already calls the library has not reimplemented it.
#[test]
fn calling_the_library_is_not_reimplementing_it() {
    let library = javascript(
        "function ArrayFind(predicate) {
           for (var k = 0; k < this.length; k++) {
             var v = this[k];
             if (callContentFunction(predicate, undefined, v, k, this)) { return v; }
           }
           return undefined;
         }",
        "ArrayFind",
    );
    let caller = typescript(
        "export function findIt(xs: string[], t: string) { return xs.find(x => x === t); }",
        "findIt",
    );
    assert!(
        !caller.matches(&library) && !caller.contains(&library),
        "already using the API must not be reported: {caller}"
    );
}

/// Searching and testing are different behaviors and must stay different.
///
/// `find` returns the element and `some` returns whether there was one. They
/// differ only in what the escape carries, which is exactly the kind of
/// distinction a normalizer erases by accident.
#[test]
fn distinct_builtins_derive_distinct_forms() {
    let source = "function ArrayFind(p) {
        for (var k = 0; k < this.length; k++) { if (p(this[k])) { return this[k]; } }
        return undefined;
      }
      function ArraySome(p) {
        for (var k = 0; k < this.length; k++) { if (p(this[k])) { return true; } }
        return false;
      }
      function ArrayEvery(p) {
        for (var k = 0; k < this.length; k++) { if (!p(this[k])) { return false; } }
        return true;
      }";
    let find = javascript(source, "ArrayFind");
    let some = javascript(source, "ArraySome");
    let every = javascript(source, "ArrayEvery");
    assert_ne!(
        find, some,
        "returning the element is not returning that there was one"
    );
    assert_ne!(some, every, "{some}");
    assert_ne!(find, every);
}

/// Which named constant an argument is, is behavior.
///
/// `keys`, `values` and `entries` are one call to one iterator factory
/// distinguished only by the constant they pass. Reading that constant as a
/// hole made all three the same behavior.
#[test]
fn a_named_constant_distinguishes_otherwise_identical_delegations() {
    let source = "function ArrayKeys() { return CreateArrayIterator(this, ITEM_KIND_KEY); }
                  function ArrayValues() { return CreateArrayIterator(this, ITEM_KIND_VALUE); }";
    assert_ne!(
        javascript(source, "ArrayKeys"),
        javascript(source, "ArrayValues"),
        "the constant is the whole difference"
    );
}

/// Delegating to different helpers is doing different things.
#[test]
fn a_delegation_keeps_the_name_it_delegates_to() {
    let source = "function a(x) { return leftHelper(x); }
                  function b(x) { return rightHelper(x); }";
    assert_ne!(javascript(source, "a"), javascript(source, "b"));
}

/// A guard for a sparse array asks nothing about the element's value.
#[test]
fn a_presence_test_inside_a_walk_is_not_behavior() {
    let guarded = javascript(
        "function f(p) {
           for (var k = 0; k < this.length; k++) { if (k in this) { if (p(this[k])) { return true; } } }
           return false;
         }",
        "f",
    );
    let plain = javascript(
        "function f(p) {
           for (var k = 0; k < this.length; k++) { if (p(this[k])) { return true; } }
           return false;
         }",
        "f",
    );
    assert_eq!(guarded, plain, "{guarded}");
}

/// A body the grammar could not read is declined rather than half-derived.
#[test]
fn a_function_the_grammar_failed_on_is_marked() {
    // SpiderMonkey preprocesses its self-hosted JavaScript, so a few bodies
    // carry `#if` lines no JavaScript grammar reads.
    let source = "function ok(x) { return x; }
                  function preprocessed(x) {
                    #if FEATURE
                    return x;
                    #endif
                  }";
    let file = parse_allowing_errors("javascript", "library.js", source);
    let functions = normalize_file(&file);
    let damaged = |name: &str| {
        functions
            .iter()
            .find(|found| found.name == name)
            .map(|found| found.damaged)
    };
    assert_eq!(damaged("ok"), Some(false));
    assert_eq!(
        damaged("preprocessed"),
        Some(true),
        "the damage must be visible"
    );
}

/// Every spelling of "the first one that matched" is one behavior.
///
/// These are the shapes `prefer-find` and `prefer-array-find` actually fire on.
/// They are chains, not loops, and until they reduced to the search they
/// describe, nothing the corpus asks for could be found at all.
#[test]
fn taking_the_first_filtered_element_is_a_search_however_it_is_spelled() {
    let loop_written = typescript(
        "export function f(xs: string[], t: string) {
           for (const x of xs) { if (x === t) { return x; } }
           return undefined;
         }",
        "f",
    );
    for spelling in [
        "return xs.filter(x => x === t)[0];",
        "return xs.filter(x => x === t)['0'];",
        "return xs.filter(x => x === t).at(0);",
        "return xs.filter(x => x === t).shift();",
    ] {
        let chained = typescript(
            &format!("export function f(xs: string[], t: string) {{ {spelling} }}"),
            "f",
        );
        assert_eq!(
            chained, loop_written,
            "{spelling} describes the same search"
        );
    }
}

/// Taking a different element is a different question.
///
/// `filter(p)[1]` and `filter(p).at(1)` are the corpus's own annotated
/// negatives, and `filter(p).pop()` is a *backwards* search — reporting it as
/// `find` would name the wrong API, which is the mistake that once made a
/// reverse iterator outrank the forward one.
#[test]
fn taking_any_other_element_is_not_that_search() {
    let search = typescript(
        "export function f(xs: string[], t: string) { return xs.filter(x => x === t)[0]; }",
        "f",
    );
    for spelling in [
        "return xs.filter(x => x === t)[1];",
        "return xs.filter(x => x === t).at(1);",
        "return xs.filter(x => x === t).pop();",
        "return xs.filter(x => x === t).at(-1);",
    ] {
        let other = typescript(
            &format!("export function f(xs: string[], t: string) {{ {spelling} }}"),
            "f",
        );
        assert_ne!(other, search, "{spelling} is not a forwards search");
        assert!(!other.matches(&search), "{spelling} must not match");
    }
}

/// Searching backwards is a different question from searching forwards.
///
/// `filter(p).pop()`, `filter(p).at(-1)` and a loop that counts down all ask
/// for the LAST match. The direction lives inside the traversal so that a
/// pattern can see it: expressed as a reversal wrapped around the sequence it
/// would sit where the derived behavior has a hole, and a hole absorbs
/// anything, so `find` would match `findLast` code and name the opposite API.
#[test]
fn a_backwards_search_is_not_a_forwards_one() {
    let forwards = typescript(
        "export function f(xs: string[], t: string) {
           for (const x of xs) { if (x === t) { return x; } }
           return undefined;
         }",
        "f",
    );
    let backwards = typescript(
        "export function f(xs: string[], t: string) {
           for (var k = xs.length - 1; k >= 0; k--) { if (xs[k] === t) { return xs[k]; } }
           return undefined;
         }",
        "f",
    );
    assert_ne!(forwards, backwards, "{forwards}");
    assert!(
        !backwards.matches(&forwards),
        "a backwards search must not match find"
    );
    assert!(
        !forwards.matches(&backwards),
        "a forwards search must not match findLast"
    );

    // every spelling of "the last one that matched" is that one behavior
    for spelling in [
        "return xs.filter(x => x === t).pop();",
        "return xs.filter(x => x === t).at(-1);",
    ] {
        let chained = typescript(
            &format!("export function f(xs: string[], t: string) {{ {spelling} }}"),
            "f",
        );
        assert_eq!(chained, backwards, "{spelling} searches backwards");
        assert!(
            !chained.matches(&forwards),
            "{spelling} must not match find"
        );
    }
}

/// A step's span has to line up with the step, not with the source statement
/// that happened to sit in that position.
///
/// Normalization drops statements outright — a guard that only throws, a
/// binding nothing reads — so counting source statements separately would slide
/// every later span by one and report a match at the wrong line.
#[test]
fn spans_stay_with_the_steps_that_survive_normalization() {
    let file = parse(
        "typescript",
        "caller.ts",
        "export function f(xs: number[], p: unknown) {\n\
         \x20 if (!p) { throw new Error('no'); }\n\
         \x20 const unused = 1;\n\
         \x20 const total = xs.length;\n\
         \x20 return total;\n\
         }",
    );
    let function = infact_ts_normalize::normalize_file(&file)
        .into_iter()
        .find(|found| found.name == "f")
        .expect("f");
    let Form::Sequence(steps) = &function.form else {
        panic!("expected a sequence, got {}", function.form);
    };
    assert_eq!(
        steps.len(),
        function.statements.len(),
        "one span per surviving step: {} steps, {} spans",
        steps.len(),
        function.statements.len()
    );
    // the throw guard and the unused binding are gone, so the first surviving
    // step is the `const total` on line 4 — not line 2
    assert_eq!(
        function.statements.first().map(|span| span.start_line),
        Some(4),
        "the first step is the first statement that survived, {:?}",
        function.statements
    );
}

/// Reversing and then searching is searching backwards.
#[test]
fn reversing_before_a_search_is_a_backwards_search() {
    let reversed = typescript(
        "export function f(xs: string[], t: string) { return [...xs].reverse().find(x => x === t); }",
        "f",
    );
    let backwards = typescript(
        "export function f(xs: string[], t: string) { return xs.filter(x => x === t).pop(); }",
        "f",
    );
    assert_eq!(
        reversed, backwards,
        "`reverse().find(p)` asks what `filter(p).pop()` asks:\n  {reversed}\n  {backwards}"
    );
    assert!(
        reversed.to_string().contains("traverse-back"),
        "and it walks backwards: {reversed}"
    );
}

/// A behavior nested deep inside a function is reported where it is written.
///
/// Saying which function contains a match is not much help when the function is
/// four hundred lines, and matching against one enormous form is both slower and
/// less precise than matching against the statement that carries the behavior.
#[test]
fn every_statement_carries_its_own_form_and_span() {
    let file = parse(
        "typescript",
        "caller.ts",
        "export function outer(xs: string[], t: string) {\n\
         \x20 const ready = true;\n\
         \x20 if (ready) {\n\
         \x20   for (const x of xs) {\n\
         \x20     if (x === t) { return x; }\n\
         \x20   }\n\
         \x20 }\n\
         \x20 return undefined;\n\
         }",
    );
    let function = infact_ts_normalize::normalize_file(&file)
        .into_iter()
        .find(|found| found.name == "outer")
        .expect("outer");

    // the walk is on line 4, three levels in, and that is where it is reported
    let walk = function
        .located
        .iter()
        .find(|located| located.form.simplify().to_string().contains("traverse"))
        .expect("the traversal was located");
    assert_eq!(
        walk.span.start_line,
        4,
        "the walk is reported at its own line, not the function's: {:?}",
        function
            .located
            .iter()
            .map(|l| (l.span.start_line, l.depth))
            .collect::<Vec<_>>()
    );
    assert!(walk.depth > 0, "and it knows it is nested");
    assert!(
        walk.form.size() < function.form.size(),
        "a statement's form is smaller than the whole function's"
    );
}

/// A counted `while` walks a sequence, and reads as the same walk a `for` does.
///
/// `while (++index < length)` is how a great deal of JavaScript iterates —
/// lodash writes 45 of its files that way — with the step folded into the test
/// and the initializer in front of the loop.
#[test]
fn a_counted_while_is_the_same_walk_as_a_counted_for() {
    let while_written = javascript(
        "function f(xs, p) {
           var index = -1, length = xs.length;
           while (++index < length) { if (p(xs[index])) { return true; } }
           return false;
         }",
        "f",
    );
    let for_written = javascript(
        "function f(xs, p) {
           for (var k = 0; k < xs.length; k++) { if (p(xs[k])) { return true; } }
           return false;
         }",
        "f",
    );
    assert_eq!(
        while_written, for_written,
        "the step being in the test does not change what is walked:\n  {while_written}\n  {for_written}"
    );
}

/// A `while` that counts nothing is still not a walk.
#[test]
fn an_uncounted_while_is_left_alone() {
    let form = javascript(
        "function f(queue) { while (queue.length) { queue.pop(); } return queue; }",
        "f",
    );
    assert!(
        !form.to_string().contains("traverse"),
        "a loop with no counter describes no traversal: {form}"
    );
}
