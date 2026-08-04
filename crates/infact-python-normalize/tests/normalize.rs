#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The normalizations that let two spellings of one behavior meet.
//!
//! Each fixture is written to exercise one rule rather than quoted from a
//! library, so the test states the rule instead of a source.

use std::path::PathBuf;

use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime};
use infact_normalize::{Direction, Form};
use infact_python_normalize::{normalize_file, normalize_module};

fn parse(name: &str, source: &str) -> ParsedFile {
    let discovery = ParserCatalog::discover([
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs")
    ]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let path = PathBuf::from(name);
    let pack = discovery
        .catalog
        .resolve("python", &path)
        .expect("no python parser pack")
        .clone();
    let file = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("load parser")
        .parse(path, source.as_bytes().to_vec())
        .expect("parse");
    assert!(!file.tree.root_node().has_error(), "{name} did not parse");
    file
}

fn form(source: &str, function: &str) -> Form {
    normalize_file(&parse("module.py", source))
        .into_iter()
        .find(|found| found.name == function)
        .unwrap_or_else(|| panic!("{function} not found"))
        .form
        .simplify()
        .canonical()
}

// -- Rule 1: building a sequence ---------------------------------------------

/// The single most important rule: an append loop and a comprehension are one
/// thing.
///
/// If these normalized differently, nothing else in the crate would matter —
/// the two are how Python's own standard library writes the same operation, and
/// which one an author picked is a matter of taste.
#[test]
fn an_append_loop_and_a_comprehension_are_the_same_sequence() {
    let looped = form(
        "def f(xs):
    out = []
    for x in xs:
        out.append(g(x))
    return out
",
        "f",
    );
    let comprehended = form(
        "def f(xs):
    return [g(x) for x in xs]
",
        "f",
    );
    assert_eq!(
        looped, comprehended,
        "appending in a loop and comprehending describe one sequence"
    );
}

#[test]
fn a_guarded_append_loop_and_a_filtered_comprehension_agree() {
    let looped = form(
        "def f(xs):
    out = []
    for x in xs:
        if p(x):
            out.append(g(x))
    return out
",
        "f",
    );
    let comprehended = form(
        "def f(xs):
    return [g(x) for x in xs if p(x)]
",
        "f",
    );
    assert_eq!(looped, comprehended);
}

/// Two guards in a comprehension and two nested `if`s in a loop are one test.
#[test]
fn stacked_conditions_are_one_condition() {
    let looped = form(
        "def f(xs):
    out = []
    for x in xs:
        if p(x):
            if q(x):
                out.append(x)
    return out
",
        "f",
    );
    let comprehended = form(
        "def f(xs):
    return [x for x in xs if p(x) if q(x)]
",
        "f",
    );
    assert_eq!(looped, comprehended);
}

/// A copy is not a transformation.
///
/// `[x for x in xs]` produces the elements it was given, and building a
/// `Transform` whose body is the item would make a copy look like work — and
/// would make it equal to `[g(x) for x in xs]` once `g` reduced to a hole.
#[test]
fn a_comprehension_that_transforms_nothing_is_not_a_transform() {
    let copied = form("def f(xs):\n    return [x for x in xs]\n", "f");
    let mapped = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    assert_ne!(copied, mapped);
    assert!(
        matches!(&copied, Form::Collect { sequence, .. } if !matches!(sequence.as_ref(), Form::Transform { .. })),
        "{copied}"
    );
}

/// Which container is being built is behavior: a set drops duplicates and a
/// list does not.
#[test]
fn a_list_and_a_set_comprehension_are_different_behaviors() {
    let list = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    let set = form("def f(xs):\n    return {g(x) for x in xs}\n", "f");
    assert_ne!(list, set);
}

/// A generator produces the sequence and a comprehension gathers it, which is
/// the whole difference between them.
#[test]
fn a_generator_is_not_a_gathered_sequence() {
    let generator = form("def f(xs):\n    return (g(x) for x in xs)\n", "f");
    let list = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    assert_ne!(generator, list);
    assert!(matches!(generator, Form::Transform { .. }), "{generator}");
}

/// A loop that also reads what it fills is not a comprehension.
///
/// `out.append(out[-1] + x)` is a running total, and rewriting it as a
/// transformation would say the elements were independent when each depends on
/// the one before.
#[test]
fn a_loop_that_reads_what_it_fills_is_left_alone() {
    let running = form(
        "def f(xs):
    out = []
    for x in xs:
        out.append(out[-1] + x)
    return out
",
        "f",
    );
    let mapped = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    assert_ne!(running, mapped);
    assert!(
        format!("{running}").contains("traverse"),
        "the walk survives: {running}"
    );
}

/// A container something later reads is a container, not a comprehension.
#[test]
fn a_container_used_after_the_walk_is_not_fused() {
    let form = form(
        "def f(xs):
    out = []
    for x in xs:
        out.append(g(x))
    out.sort()
    return out
",
        "f",
    );
    assert!(format!("{form}").contains("traverse"), "{form}");
}

// -- Rule 2: failure ---------------------------------------------------------

/// Python spells a fallible operation with `try`, and Rust with a `match` on a
/// `Result`. Both are a decision about what went wrong.
#[test]
fn a_try_except_is_a_decision_about_what_failed() {
    let form = form(
        "def f(d, k):
    try:
        return d[k]
    except KeyError:
        return None
",
        "f",
    );
    assert!(
        matches!(&form, Form::Select { arms, .. } if arms.len() == 1),
        "{form}"
    );
    assert!(format!("{form}").contains("KeyError"), "{form}");
}

/// Which exception is caught is behavior. Recovering from a missing key and
/// recovering from a bad value are not the same operation, and a form that
/// erased the name would report one as the other.
#[test]
fn catching_different_exceptions_is_different_behavior() {
    let key = form(
        "def f(d, k):
    try:
        return d[k]
    except KeyError:
        return None
",
        "f",
    );
    let value = form(
        "def f(d, k):
    try:
        return d[k]
    except ValueError:
        return None
",
        "f",
    );
    assert_ne!(key, value);
}

/// A bare `except:` catches more than `except Exception:` does, and says so.
#[test]
fn a_bare_except_is_a_wider_claim_than_a_named_one() {
    let bare = form(
        "def f(d, k):
    try:
        return d[k]
    except:
        return None
",
        "f",
    );
    let named = form(
        "def f(d, k):
    try:
        return d[k]
    except Exception:
        return None
",
        "f",
    );
    assert_ne!(bare, named);
    assert!(format!("{bare}").contains("BaseException"), "{bare}");
}

/// Handlers written in either order are one decision, because `Select` holds
/// its arms sorted by what they name.
#[test]
fn handlers_written_in_either_order_are_one_decision() {
    let first = form(
        "def f(x):
    try:
        return g(x)
    except KeyError:
        return 1
    except ValueError:
        return 2
",
        "f",
    );
    let second = form(
        "def f(x):
    try:
        return g(x)
    except ValueError:
        return 2
    except KeyError:
        return 1
",
        "f",
    );
    assert_eq!(first, second);
}

// -- Rule 3: preconditions ---------------------------------------------------

/// A guard that only raises states what the caller must not do. Every
/// implementation writes them and no reimplementation does, so a form that kept
/// them could never match the code it describes.
#[test]
fn a_guard_that_only_raises_is_not_behavior() {
    let guarded = form(
        "def f(xs):
    if not xs:
        raise ValueError('empty')
    assert len(xs) > 0
    return [g(x) for x in xs]
",
        "f",
    );
    let plain = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    assert_eq!(guarded, plain);
}

/// A guard that does anything else is ordinary behavior and stays.
#[test]
fn a_guard_that_returns_is_still_behavior() {
    let guarded = form(
        "def f(xs):
    if not xs:
        return None
    return [g(x) for x in xs]
",
        "f",
    );
    let plain = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    assert_ne!(guarded, plain);
}

// -- Direction ---------------------------------------------------------------

/// Searching from the front and from the back are different questions.
///
/// This is the mistake the Rust normalizer paid for once, where a reverse
/// iterator outranked the forward one and every `find` was reported as
/// `rfind`. Peeling `reversed` without recording it would repeat it.
#[test]
fn walking_backwards_is_not_walking_forwards() {
    let forwards = form(
        "def f(xs):
    for x in xs:
        if p(x):
            return x
",
        "f",
    );
    let backwards = form(
        "def f(xs):
    for x in reversed(xs):
        if p(x):
            return x
",
        "f",
    );
    assert_ne!(forwards, backwards);
    assert!(
        matches!(&backwards, Form::Traverse { direction, .. } if *direction == Direction::Backward),
        "{backwards}"
    );
}

/// `for x in list(xs)` and `for x in xs` walk the same elements.
#[test]
fn an_adapter_in_the_sequence_position_says_nothing() {
    let adapted = form("def f(xs):\n    for x in list(xs):\n        g(x)\n", "f");
    let plain = form("def f(xs):\n    for x in xs:\n        g(x)\n", "f");
    assert_eq!(adapted, plain);
}

/// But `list(xs)` standing on its own is a real allocation.
#[test]
fn an_adapter_outside_a_walk_is_a_gathering() {
    let form = form("def f(xs):\n    return list(xs)\n", "f");
    assert!(matches!(form, Form::Collect { .. }), "{form}");
}

// -- Ordinary syntax ---------------------------------------------------------

/// `a < b < c` is `a < b and b < c`, and Python is the only language here that
/// spells it in one node.
#[test]
fn a_chained_comparison_is_the_conjunction_it_stands_for() {
    let chained = form("def f(a, b, c):\n    return a < b < c\n", "f");
    let written = form("def f(a, b, c):\n    return a < b and b < c\n", "f");
    assert_eq!(chained, written);
}

/// `not in` and `is not` are two tokens and one operator.
#[test]
fn a_two_token_operator_is_read_whole() {
    let form = form("def f(x, xs):\n    return x not in xs\n", "f");
    assert!(format!("{form}").contains("not in"), "{form}");
    let identity = self::form("def f(x):\n    return x is not None\n", "f");
    assert!(format!("{identity}").contains("is not"), "{identity}");
}

/// `elif` is `else: if`, and one decision written two ways is one form.
#[test]
fn an_elif_and_a_nested_else_are_one_decision() {
    let chained = form(
        "def f(x):
    if a(x):
        return 1
    elif b(x):
        return 2
    else:
        return 3
",
        "f",
    );
    let nested = form(
        "def f(x):
    if a(x):
        return 1
    else:
        if b(x):
            return 2
        else:
            return 3
",
        "f",
    );
    assert_eq!(chained, nested);
}

/// The grammar writes a conditional's consequence FIRST and names none of the
/// three parts, so they can only be read positionally — and reading them wrong
/// would invert every conditional in the language without failing to parse.
#[test]
fn a_conditional_expression_reads_its_parts_in_the_right_order() {
    let form = form("def f(x):\n    return 1 if c(x) else 2\n", "f");
    let Form::Branch {
        condition,
        consequence,
        alternative,
    } = &form
    else {
        panic!("{form}");
    };
    assert!(matches!(condition.as_ref(), Form::Call { .. }), "{form}");
    assert_eq!(
        consequence.as_ref(),
        &Form::Number("1".to_owned()),
        "{form}"
    );
    assert_eq!(
        alternative.as_deref(),
        Some(&Form::Number("2".to_owned())),
        "{form}"
    );
}

/// A `match` is a decision, exactly as it is in Rust.
#[test]
fn a_match_statement_is_a_decision() {
    let form = form(
        "def f(command):
    match command:
        case ['go', direction]:
            return direction
        case _:
            return None
",
        "f",
    );
    assert!(
        matches!(&form, Form::Select { arms, .. } if arms.len() == 2),
        "{form}"
    );
}

/// A capture pattern binds; a capitalised or dotted one compares.
///
/// Reading `case x:` as a name would make it a different arm from `case y:`
/// while both match everything, and a catch-all that sorted like a named arm
/// would stop being last.
#[test]
fn a_capture_pattern_binds_rather_than_naming() {
    let left = form(
        "def f(v):
    match v:
        case Point():
            return 1
        case x:
            return x
",
        "f",
    );
    let right = form(
        "def f(v):
    match v:
        case Point():
            return 1
        case y:
            return y
",
        "f",
    );
    assert_eq!(left, right, "the capture's name is not behavior");
}

/// `with open(p) as f` binds `f` and then does the work.
#[test]
fn a_context_manager_is_a_binding_and_a_body() {
    let managed = form(
        "def f(p):
    with open(p) as fh:
        return fh.read()
",
        "f",
    );
    let plain = form(
        "def f(p):
    fh = open(p)
    return fh.read()
",
        "f",
    );
    assert_eq!(managed, plain);
}

/// The walrus binds and yields in one breath.
#[test]
fn a_walrus_binds_the_name_it_introduces() {
    let form = form(
        "def f(xs):
    if (n := len(xs)) > 10:
        return n
    return 0
",
        "f",
    );
    assert!(format!("{form}").contains("let"), "{form}");
}

/// `None` is the absence a caller has to handle, which is what `Option::None`
/// and `undefined` are in the other frontends.
#[test]
fn none_is_the_same_absence_the_other_languages_spell() {
    let form = form("def f():\n    return None\n", "f");
    assert!(
        matches!(&form, Form::Variant { name, .. } if name == "None"),
        "{form}"
    );
}

/// Which identifier an author chose is not behavior.
#[test]
fn renaming_everything_changes_nothing() {
    let left = form(
        "def f(items):
    total = 0
    for item in items:
        total += item
    return total
",
        "f",
    );
    let right = form(
        "def f(xs):
    acc = 0
    for x in xs:
        acc += x
    return acc
",
        "f",
    );
    assert_eq!(left, right);
}

/// Annotations, decorators and docstrings describe code; they are not it.
#[test]
fn annotation_is_not_behavior() {
    let decorated = form(
        "@cache
def f(xs: list[int]) -> list[int]:
    'Double everything.'
    return [g(x) for x in xs]
",
        "f",
    );
    let plain = form("def f(xs):\n    return [g(x) for x in xs]\n", "f");
    assert_ne!(
        decorated, plain,
        "a docstring is an expression statement and stays visible"
    );
    let undocumented = form(
        "@cache
def f(xs: list[int]) -> list[int]:
    return [g(x) for x in xs]
",
        "f",
    );
    assert_eq!(undocumented, plain);
}

/// A module's top level is code that runs, and a normalizer that only read
/// functions would report nothing about a script.
#[test]
fn a_module_body_is_normalized_too() {
    let file = parse(
        "script.py",
        "import os

out = []
for x in source():
    out.append(g(x))
print(out)
",
    );
    let module = normalize_module(&file).simplify().canonical();
    assert!(format!("{module}").contains("collect"), "{module}");
}

/// A method is a function definition inside a class body, and a helper is one
/// inside another function. Both are found.
#[test]
fn methods_and_nested_helpers_are_both_collected() {
    let file = parse(
        "module.py",
        "class C:
    def method(self):
        def helper():
            return 1
        return helper()
",
    );
    let mut names: Vec<_> = normalize_file(&file)
        .into_iter()
        .map(|found| found.name)
        .collect();
    names.sort();
    assert_eq!(names, ["helper", "method"]);
}

/// A function's span covers its decorators, because that is what a reader would
/// be pointed at.
#[test]
fn a_decorated_function_is_located_at_its_decorator() {
    let source = "@register\ndef f():\n    return 1\n";
    let file = parse("module.py", source);
    let found = normalize_file(&file).into_iter().next().expect("f");
    assert_eq!(found.start_line, 1);
    assert_eq!(found.start_byte, 0);
}

/// Every statement is recorded where it was written, so a match found deep
/// inside a function can be reported at the line that carries it.
#[test]
fn every_statement_keeps_its_own_span() {
    let source = "def f(xs):
    a = 1
    for x in xs:
        g(x)
    return a
";
    let file = parse("module.py", source);
    let found = normalize_file(&file).into_iter().next().expect("f");
    assert!(
        found.located.iter().any(|located| located.depth > 0),
        "statements inside the walk are located too"
    );
    for located in &found.located {
        let quoted = &source[located.span.start_byte as usize..located.span.end_byte as usize];
        assert!(!quoted.is_empty(), "{located:?}");
    }
}

/// `xs[1:]` and `xs[:1]` are opposite halves of a sequence.
///
/// A slice held as one opaque node was 72% of everything this crate could not
/// read across CPython's standard library, and a slice that dropped its missing
/// bound would make the front and the back of a list one value.
#[test]
fn the_ends_of_a_slice_are_not_interchangeable() {
    let front = form("def f(xs):\n    return xs[:1]\n", "f");
    let back = form("def f(xs):\n    return xs[1:]\n", "f");
    assert_ne!(front, back);
    assert!(
        matches!(&front, Form::Method { name, arguments, .. } if name == "slice" && arguments.len() == 2),
        "{front}"
    );
    // An element is not a run of them.
    let element = form("def f(xs):\n    return xs[1]\n", "f");
    assert_ne!(element, back);
    let stepped = form("def f(xs):\n    return xs[::2]\n", "f");
    assert_ne!(stepped, form("def f(xs):\n    return xs[:]\n", "f"));
}

/// A function applied to itself makes unfolding produce a bigger form each
/// time. CPython's `test_inspect` writes the Y combinator, which is the
/// shortest way to reach it, and simplifying it aborted the process before the
/// core learned to bound unfolding.
#[test]
fn self_application_does_not_run_forever() {
    let form = form(
        "def Y(le):
    def g(f):
        return le(lambda x: f(f)(x))
    return g(g)
",
        "Y",
    );
    assert!(form.size() > 0, "it terminated, which is the assertion");
}

/// Two local functions that call each other are the same problem one step out,
/// and CPython's `json/encoder.py` has three of them.
#[test]
fn mutually_recursive_local_functions_do_not_run_forever() {
    let form = form(
        "def make():
    def a(v):
        return b(v)
    def b(v):
        return a(v)
    return a
",
        "make",
    );
    assert!(form.size() > 0);
}

// -- Rule 12: a name that is called ------------------------------------------

/// Two delegations to different helpers are two behaviors.
///
/// This is the erasure the corpus reported rather than the fixture: measured
/// over the installed Python, 94.9% of calls had a hole for a callee, and
/// `asyncio` writes six pipe-transport factories whose bodies differ in
/// nothing but the class being constructed. Every one of them was one form.
#[test]
fn two_delegations_to_different_helpers_are_two_behaviors() {
    let read = form(
        "def f(self, pipe, protocol, waiter, extra):
    return _UnixReadPipeTransport(self, pipe, protocol, waiter, extra)
",
        "f",
    );
    let write = form(
        "def f(self, pipe, protocol, waiter, extra):
    return _UnixWritePipeTransport(self, pipe, protocol, waiter, extra)
",
        "f",
    );
    assert_ne!(
        read, write,
        "constructing a read transport is not constructing a write one"
    );
}

/// A name the caller supplied is still a hole, however it is spelled.
///
/// This is the guard on the rule above rather than a separate rule. `apply(g,
/// x)` calls whatever it was handed, and two callers may hand it anything, so
/// the name `g` says nothing and renaming it must change nothing. Resolving
/// every called identifier to a path would have broken exactly this.
#[test]
fn a_parameter_that_is_called_is_still_a_hole() {
    let applied = form(
        "def f(g, x):
    return g(x)
",
        "f",
    );
    let renamed = form(
        "def f(h, x):
    return h(x)
",
        "f",
    );
    assert_eq!(
        applied, renamed,
        "which name a parameter was given is not behavior"
    );

    let named = form(
        "def f(x):
    return helper(x)
",
        "f",
    );
    assert_ne!(
        applied, named,
        "calling what you were handed is not calling something defined elsewhere"
    );
}

/// Which constant was passed is behavior.
///
/// `infact-ts-normalize` carries the same rule and records what it cost: with
/// both constants resolved to a hole, `keys` and `values` were one behavior
/// 1,390 times over. Python spells constants the same way.
#[test]
fn a_named_constant_is_not_a_hole() {
    let read = form(
        "def f(path):
    return open_file(path, MODE_READ)
",
        "f",
    );
    let write = form(
        "def f(path):
    return open_file(path, MODE_WRITE)
",
        "f",
    );
    assert_ne!(read, write, "the mode is the behavior");
}

/// A single upper-case letter is a type variable, not a constant.
///
/// `T` and `K` appear as `TypeVar` names throughout typed Python, and treating
/// them as named constants would make two identically-shaped generic helpers
/// differ over nothing a consumer could report.
#[test]
fn a_single_letter_name_is_not_a_named_constant() {
    let left = form(
        "def f(xs):
    return cast(T, xs)
",
        "f",
    );
    let right = form(
        "def f(xs):
    return cast(K, xs)
",
        "f",
    );
    assert_eq!(left, right, "a type variable is not behavior");
}
