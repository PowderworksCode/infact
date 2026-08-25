#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The normalizer's contract: implementations that behave alike normalize
//! alike, implementations that behave differently do not, and no rule here
//! knows about any particular library.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use entl_tree_sitter::{ParserPack, ParserRuntime};
use infact_rust_normalize::{normalize_body, normalize_file};

fn parser_packs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs/rust")
}

fn form_of(source: &str, function: &str) -> String {
    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let parsed = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser")
        .parse("source.rs", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source");
    let functions = normalize_file(&parsed);
    functions
        .into_iter()
        .find(|candidate| candidate.name == function)
        .unwrap_or_else(|| panic!("no function named {function}"))
        .form
        .to_string()
}

/// The central claim: a hand-written loop and a combinator implementation of
/// the same behavior reduce to one form.
#[test]
fn a_loop_and_a_combinator_agree() {
    let loop_form = form_of(
        "fn counts(values: Vec<String>) -> HashMap<String, usize> {
             let mut counts = HashMap::<String, usize>::new();
             for value in values {
                 *counts.entry(value).or_default() += 1;
             }
             counts
         }",
        "counts",
    );
    let combinator_form = form_of(
        "fn counts_with_hasher<S>(self, hash_builder: S) -> HashMap<Self::Item, usize, S> {
             let mut counts = HashMap::with_hasher(hash_builder);
             self.for_each(|item| *counts.entry(item).or_default() += 1);
             counts
         }",
        "counts_with_hasher",
    );
    assert_eq!(loop_form, combinator_form);
    assert_eq!(
        loop_form,
        "(do (let v0 (construct HashMap)) \
         (traverse f0 v1 (assign += (method or_default (method entry v0 v1)) (num 1))) v0)"
    );
}

/// The same agreement holds through tuple destructuring.
#[test]
fn destructured_items_agree_across_forms() {
    let loop_form = form_of(
        "fn group(pairs: Vec<(String, u32)>) -> HashMap<String, Vec<u32>> {
             let mut lookup = HashMap::new();
             for (key, value) in pairs {
                 lookup.entry(key).or_default().push(value);
             }
             lookup
         }",
        "group",
    );
    let combinator_form = form_of(
        "fn into_group_map_with_hasher<I, K, V, S>(iter: I, hash_builder: S) -> HashMap<K, Vec<V>, S> {
             let mut lookup = HashMap::<K, Vec<V>, S>::with_hasher(hash_builder);
             iter.for_each(|(key, val)| {
                 lookup.entry(key).or_default().push(val);
             });
             lookup
         }",
        "into_group_map_with_hasher",
    );
    assert_eq!(loop_form, combinator_form);
}

/// Normalization must not flatten distinct behavior together, or every match
/// would be a false positive.
#[test]
fn different_behavior_keeps_different_forms() {
    let counting = form_of(
        "fn counting(values: Vec<String>) -> HashMap<String, usize> {
             let mut counts = HashMap::new();
             for value in values {
                 *counts.entry(value).or_default() += 1;
             }
             counts
         }",
        "counting",
    );
    let summing = form_of(
        "fn summing(pairs: Vec<(String, u32)>) -> HashMap<String, u32> {
             let mut totals = HashMap::new();
             for (key, value) in pairs {
                 *totals.entry(key).or_default() += value;
             }
             totals
         }",
        "summing",
    );
    let collecting = form_of(
        "fn collecting(values: Vec<String>) -> Vec<String> {
             let mut output = Vec::new();
             for value in values {
                 output.push(value);
             }
             output
         }",
        "collecting",
    );
    assert_ne!(counting, summing, "counting is not summing");
    assert_ne!(counting, collecting, "counting is not collecting");
    assert_ne!(summing, collecting, "summing is not collecting");
}

#[test]
fn identity_adapters_and_turbofish_do_not_change_behavior() {
    let plain = form_of(
        "fn plain(values: Vec<u32>) -> Vec<u32> {
             let mut output = Vec::new();
             for value in values {
                 output.push(value);
             }
             output
         }",
        "plain",
    );
    let adapted = form_of(
        "fn adapted(values: Vec<u32>) -> Vec<u32> {
             let mut output = Vec::<u32>::with_capacity(8);
             for value in values.iter().copied().by_ref() {
                 output.push(value);
             }
             output
         }",
        "adapted",
    );
    assert_eq!(plain, adapted);
}

#[test]
fn transform_retain_and_accumulate_are_distinct_operations() {
    let mapped = form_of(
        "fn mapped(v: Vec<u32>) { v.into_iter().map(|x| x + 1); }",
        "mapped",
    );
    let filtered = form_of(
        "fn filtered(v: Vec<u32>) { v.into_iter().filter(|x| x > 1); }",
        "filtered",
    );
    let folded = form_of(
        "fn folded(v: Vec<u32>) { v.into_iter().fold(0, |acc, x| acc + x); }",
        "folded",
    );
    assert!(mapped.contains("(transform "), "{mapped}");
    assert!(filtered.contains("(retain "), "{filtered}");
    assert!(folded.contains("(accumulate "), "{folded}");
}

#[test]
fn a_collect_records_its_container_when_the_turbofish_names_one() {
    let annotated = form_of(
        "fn annotated(v: Vec<u32>) -> Vec<u32> { v.into_iter().collect::<Vec<_>>() }",
        "annotated",
    );
    assert!(annotated.contains("(collect f0 Vec)"), "{annotated}");
    let inferred = form_of(
        "fn inferred(v: Vec<u32>) -> Vec<u32> { v.into_iter().collect() }",
        "inferred",
    );
    assert!(inferred.contains("(collect f0)"), "{inferred}");
}

/// Behavior is found inside a larger function, not only as a whole body.
#[test]
fn a_behavior_is_located_inside_surrounding_code() {
    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let runtime = ParserRuntime::new().expect("parser runtime");
    let parser = runtime.load(pack).expect("loading rust parser");

    let library = "fn counts(self) -> HashMap<T, usize> {
        let mut counts = HashMap::new();
        self.for_each(|item| *counts.entry(item).or_default() += 1);
        counts
    }";
    let repository = "fn report(&self, values: Vec<String>) -> String {
        let header = self.title();
        let mut counts = HashMap::new();
        for value in values {
            *counts.entry(value).or_default() += 1;
        }
        format!(\"{} {:?}\", header, counts)
    }";

    let parse = |name: &str, source: &str| {
        parser
            .parse(name, Arc::<[u8]>::from(source.as_bytes()))
            .expect("parsing source")
    };
    let library = parse("library.rs", library);
    let repository = parse("repository.rs", repository);

    let behavior = normalize_file(&library).remove(0).form;
    let body = normalize_file(&repository).remove(0).form;

    // the library body is a sequence ending in the accumulator; the repository
    // reuses the same traversal inside a larger function
    let traversal = behavior
        .children()
        .into_iter()
        .find(|child| matches!(child, infact_normalize::Form::Traverse { .. }))
        .expect("library body traverses")
        .clone();
    assert!(
        body.contains(&traversal),
        "expected to find\n  {traversal}\ninside\n  {body}"
    );
}

#[test]
fn every_function_in_a_file_is_normalized() {
    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let source = "fn one() { let a = 1; }\nfn two() { let b = 2; }\nimpl T { fn three(&self) {} }";
    let parsed = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser")
        .parse("source.rs", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source");
    let functions = normalize_file(&parsed);
    let names = functions
        .iter()
        .map(|function| function.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["one", "two", "three"]);
    assert!(functions.iter().all(|function| function.start_line >= 1));
}

#[test]
fn normalizing_a_body_directly_matches_normalizing_its_file() {
    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let source = "fn only(values: Vec<u32>) { for value in values { drop(value); } }";
    let parsed = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser")
        .parse("source.rs", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source");
    let from_file = normalize_file(&parsed).remove(0).form;
    let function = parsed
        .tree
        .root_node()
        .child(0)
        .expect("a function item")
        .child_by_field_name("body")
        .expect("a body");
    let direct = normalize_body(function, &parsed.source);
    assert_eq!(from_file, direct);
    let _ = Path::new("unused");
}

/// The normalized, simplified, consistently-renamed form of one function.
///
/// Comparing two spellings means comparing what a match would compare, which is
/// the simplified form up to renaming rather than the form as written.
fn behavior_of(source: &str, function: &str) -> String {
    let pack = Arc::new(ParserPack::load(parser_packs()).expect("rust parser pack"));
    let parsed = ParserRuntime::new()
        .expect("parser runtime")
        .load(pack)
        .expect("loading rust parser")
        .parse("source.rs", Arc::<[u8]>::from(source.as_bytes()))
        .expect("parsing source");
    normalize_file(&parsed)
        .into_iter()
        .find(|candidate| candidate.name == function)
        .unwrap_or_else(|| panic!("no function named {function}"))
        .form
        .simplify()
        .canonical()
        .to_string()
}

/// A test and its opposite are not the same behavior.
///
/// `unary_expression` used to be stripped as noise, so these two forms were
/// equal and a library that returns on one matched code that returns on the
/// other.
#[test]
fn a_negated_test_differs_from_the_test() {
    let negated = behavior_of(
        "fn f(values: &[i32]) -> bool {
             let mut seen = HashSet::new();
             for value in values { if !seen.insert(value) { return false; } }
             true
         }",
        "f",
    );
    let plain = behavior_of(
        "fn f(values: &[i32]) -> bool {
             let mut seen = HashSet::new();
             for value in values { if seen.insert(value) { return false; } }
             true
         }",
        "f",
    );
    assert_ne!(negated, plain);
    assert!(negated.contains("(unary !"), "{negated}");
}

/// A dereference still is noise.
#[test]
fn a_dereference_is_not_behavior() {
    let starred = behavior_of("fn f(value: &i32) -> i32 { *value + 1 }", "f");
    let bare = behavior_of("fn f(value: i32) -> i32 { value + 1 }", "f");
    assert_eq!(starred, bare);
}

/// An inclusive range reaches one element further, so it is a different walk.
#[test]
fn an_inclusive_span_differs_from_an_exclusive_one() {
    let exclusive = behavior_of("fn f(n: usize) { for i in 0..n { g(i); } }", "f");
    let inclusive = behavior_of("fn f(n: usize) { for i in 0..=n { g(i); } }", "f");
    assert_ne!(exclusive, inclusive);
}

/// A loop over indices that only ever indexes is a loop over elements.
#[test]
fn an_index_loop_agrees_with_an_element_loop() {
    let indexed = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             for i in 0..values.len() { total += values[i]; }
             total
         }",
        "f",
    );
    let direct = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             for value in values { total += value; }
             total
         }",
        "f",
    );
    assert_eq!(indexed, direct);
}

/// An index used for anything but indexing is not forgotten.
///
/// `values[i + 1]` looks at a different element, so the loop is not an element
/// visit and the span has to stay.
#[test]
fn an_index_loop_that_looks_elsewhere_keeps_its_span() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             for i in 0..values.len() { total += values[i + 1]; }
             total
         }",
        "f",
    );
    assert!(form.contains("(span"), "{form}");
}

/// Writing through an index is not visiting an element.
#[test]
fn an_index_loop_that_writes_keeps_its_span() {
    let form = behavior_of(
        "fn f(values: &mut [i32]) {
             for i in 0..values.len() { values[i] = 0; }
         }",
        "f",
    );
    assert!(form.contains("(span"), "{form}");
}

/// The two spellings of a pairwise walk reduce to one form.
#[test]
fn an_index_pair_loop_agrees_with_an_enumerated_one() {
    let indexed = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in i + 1..values.len() {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    let enumerated = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for (i, a) in values.iter().enumerate() {
                 for b in values.iter().skip(i + 1) {
                     if a == b { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert_eq!(indexed, enumerated);
    assert!(indexed.contains("(pairwise"), "{indexed}");
}

/// The lower triangle visits the same pairs as the upper one.
#[test]
fn the_lower_triangle_is_also_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in 0..i {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(form.contains("(pairwise"), "{form}");
}

/// A guarded square loop over `len()` is a walk over pairs, both ways round.
///
/// This used to be refused on the grounds that the `i != j` is written over
/// indices the rewrite forgets. That was the wrong conclusion from a right
/// observation: the guard has to be CONSUMED rather than carried, and removing
/// it is sound because what it excludes is what the resulting form excludes
/// anyway. Measured against CodeNet submissions to problems that are about
/// distinctness, this spelling is a fifth of the hand-rolled ones.
#[test]
fn a_guarded_square_loop_over_a_length_is_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in 0..values.len() {
                     if i != j && values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(form.contains("(pairwise-both-ways"), "{form}");
}

/// A nested loop that touches the sequence itself is not a walk over pairs.
///
/// A bubble sort's inner body swaps through the sequence, so the elements are
/// not all it reaches.
#[test]
fn a_nested_loop_that_swaps_is_not_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &mut [i32]) {
             for i in 0..values.len() {
                 for j in 0..values.len() - 1 - i {
                     if values[j] > values[j + 1] { values.swap(j, j + 1); }
                 }
             }
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// Two spellings of one descending bound are one bound.
#[test]
fn a_subtraction_chain_has_one_spelling() {
    let first = behavior_of(
        "fn f(values: &[i32], i: usize) { for j in 0..values.len() - 1 - i { g(j); } }",
        "f",
    );
    let second = behavior_of(
        "fn f(values: &[i32], i: usize) { for j in 0..values.len() - i - 1 { g(j); } }",
        "f",
    );
    assert_eq!(first, second);
}

/// A loop bounded by a variable walks a prefix, and the form says so.
///
/// Measured on CodeNet, pairwise loops bound by a bare variable outnumber those
/// bound by `len()` six to one, so this is the common case rather than an edge.
/// The prefix is recorded because `0..n` does not reach past `n`, and claiming
/// it covered the sequence would recommend an API over elements never read.
#[test]
fn a_loop_bounded_by_a_variable_walks_a_prefix() {
    let bounded = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n {
                 for j in i + 1..n {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(bounded.contains("(pairwise (index"), "{bounded}");
    let whole = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in i + 1..values.len() {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(whole.contains("(pairwise f"), "{whole}");
    assert_ne!(bounded, whole);
}

/// Two loops bounded by different things are not a walk over pairs.
#[test]
fn loops_with_disagreeing_bounds_are_not_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32], n: usize, m: usize) -> bool {
             for i in 0..n {
                 for j in i + 1..m {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// Indexing two different sequences is a walk over neither.
#[test]
fn indexing_two_sequences_is_not_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(left: &[i32], right: &[i32], n: usize) -> bool {
             for i in 0..n {
                 for j in i + 1..n {
                     if left[i] == right[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// An outer loop that stops one short reaches the same pairs.
///
/// `0..n - 1` with `i + 1..n` pairs everything `0..n` would: the last position
/// has nothing above it. A third of the checks measured in CodeNet are written
/// this way.
#[test]
fn an_outer_loop_that_stops_one_short_is_still_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n - 1 {
                 for j in i + 1..n {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(form.contains("(pairwise"), "{form}");
    // The extent is how far the sequence is read, which is the inner bound.
    let whole = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n {
                 for j in i + 1..n {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert_eq!(form, whole);
}

/// A lower-triangle inner loop gets no such slack.
///
/// `0..n - 1` with `0..i` never pairs the last position with anything, so it is
/// not a walk over the pairs of `n` elements.
#[test]
fn a_short_outer_loop_with_a_lower_triangle_walks_a_shorter_prefix() {
    let short = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n - 1 {
                 for j in 0..i {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    let whole = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n {
                 for j in 0..i {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert_ne!(short, whole);
}

/// A square loop with the diagonal guarded away is a walk over pairs.
///
/// It reaches each pair both ways round rather than once, which the form
/// records, because a decision that does not care gets the same answer and a
/// count gets double.
#[test]
fn a_guarded_square_loop_is_a_pairwise_walk() {
    for guard in [
        "if i != j && values[i] == values[j] { return false; }",
        "if i != j { if values[i] == values[j] { return false; } }",
        "if i == j { continue; } if values[i] == values[j] { return false; }",
    ] {
        let form = behavior_of(
            &format!(
                "fn f(values: &[i32], n: usize) -> bool {{
                     for i in 0..n {{ for j in 0..n {{ {guard} }} }}
                     true
                 }}"
            ),
            "f",
        );
        assert!(form.contains("(pairwise-both-ways"), "{guard}: {form}");
    }
}

/// An unguarded square loop pairs elements with themselves and decides nothing.
#[test]
fn an_unguarded_square_loop_is_not_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n { for j in 0..n { if values[i] == values[j] { return false; } } }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// Visiting each pair twice is not the same form as visiting it once.
#[test]
fn the_two_coverages_are_different_forms() {
    let square = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n { for j in 0..n { if i != j && values[i] == values[j] { return false; } } }
             true
         }",
        "f",
    );
    let triangle = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n { for j in i + 1..n { if values[i] == values[j] { return false; } } }
             true
         }",
        "f",
    );
    assert_ne!(square, triangle);
}

/// A square loop guarded on something other than the diagonal is not this.
#[test]
fn a_square_loop_guarded_on_something_else_is_not_a_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32], n: usize) -> bool {
             for i in 0..n { for j in 0..n { if i < j && values[i] == values[j] { return false; } } }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// A loop over an interior range walks that slice, not the whole sequence.
///
/// `1..k` reads neither the first element nor anything from `k` on, so the
/// extent is `v[1..k]` — the same form the frontend gives that slice written
/// out. A prefix and an interior range are the same case and neither is the
/// whole sequence.
#[test]
fn a_loop_over_an_interior_range_walks_a_slice() {
    let interior = behavior_of(
        "fn f(values: &[i32], k: usize) -> bool {
             for i in 1..k {
                 for j in i + 1..k {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(interior.contains("(pairwise (index"), "{interior}");
    let whole = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in i + 1..values.len() {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert_ne!(interior, whole);
}

/// A lower-triangle inner loop must start where the outer one did.
///
/// `for i in 1..k { for j in 0..i }` reaches pairs involving position 0, which
/// the outer loop never visits, so the walk is not over the slice `1..k`.
#[test]
fn a_lower_triangle_that_reaches_below_the_start_is_refused() {
    let form = behavior_of(
        "fn f(values: &[i32], k: usize) -> bool {
             for i in 1..k {
                 for j in 0..i {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// A loop reading each element and its neighbour is a walk over adjacent pairs.
#[test]
fn a_neighbour_loop_is_an_adjacent_pairwise_walk() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() - 1 {
                 if values[i] > values[i + 1] { return false; }
             }
             true
         }",
        "f",
    );
    assert!(form.contains("(pairwise-adjacent"), "{form}");
}

/// Reading a neighbour two along is not a walk over adjacent pairs.
#[test]
fn a_loop_reading_two_along_is_not_adjacent() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() - 2 {
                 if values[i] > values[i + 2] { return false; }
             }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// A loop that runs to the end reads past it, so it is a different walk.
///
/// `for i in 0..v.len()` with `v[i + 1]` panics on the last step; treating it
/// as the `windows(2)` walk would report a working API for code that does not
/// work.
#[test]
fn a_neighbour_loop_that_runs_to_the_end_is_not_adjacent() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 if values[i] > values[i + 1] { return false; }
             }
             true
         }",
        "f",
    );
    assert!(!form.contains("(pairwise"), "{form}");
}

/// Adjacent pairs are not the same walk as every pair.
///
/// The same test means something much weaker over neighbours than over all
/// pairs, so the two must not reduce to one form.
#[test]
fn adjacent_pairs_differ_from_every_pair() {
    let adjacent = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() - 1 { if values[i] > values[i + 1] { return false; } }
             true
         }",
        "f",
    );
    let every = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in i + 1..values.len() { if values[i] > values[j] { return false; } }
             }
             true
         }",
        "f",
    );
    assert_ne!(adjacent, every);
}

/// An exchange written through a temporary is the exchange.
///
/// This is the difference the two spellings of a bubble sort came down to, and
/// with it they reduce to one form.
#[test]
fn a_temporary_swap_agrees_with_the_method() {
    let method = behavior_of(
        "fn f(values: &mut [i32]) {
             for i in 0..values.len() {
                 for j in 0..values.len() - 1 - i {
                     if values[j] > values[j + 1] { values.swap(j, j + 1); }
                 }
             }
         }",
        "f",
    );
    let temporary = behavior_of(
        "fn f(values: &mut [i32]) {
             for i in 0..values.len() {
                 for j in 0..values.len() - 1 - i {
                     if values[j] > values[j + 1] {
                         let t = values[j];
                         values[j] = values[j + 1];
                         values[j + 1] = t;
                     }
                 }
             }
         }",
        "f",
    );
    assert!(method.contains("(swap"), "{method}");
    assert_eq!(method, temporary);
}

/// Moving values around is not exchanging two of them.
#[test]
fn a_shift_through_a_temporary_is_not_a_swap() {
    let form = behavior_of(
        "fn f(values: &mut [i32], i: usize, j: usize, k: usize) {
             let t = values[i];
             values[i] = values[j];
             values[k] = t;
         }",
        "f",
    );
    assert!(!form.contains("(swap"), "{form}");
}

/// A temporary that is used again is not spent on the exchange.
#[test]
fn a_temporary_read_afterwards_is_not_a_swap() {
    let form = behavior_of(
        "fn f(values: &mut [i32], i: usize, j: usize) -> i32 {
             let t = values[i];
             values[i] = values[j];
             values[j] = t;
             t
         }",
        "f",
    );
    assert!(!form.contains("(swap"), "{form}");
}

/// A `loop` that tests for its own end is a `while`.
#[test]
fn a_loop_that_breaks_agrees_with_a_while() {
    let broken = behavior_of(
        "fn f(n: usize) -> usize {
             let mut i = 0;
             loop { if i >= n { break; } i += 1; }
             i
         }",
        "f",
    );
    let guarded = behavior_of(
        "fn f(n: usize) -> usize {
             let mut i = 0;
             while !(i >= n) { i += 1; }
             i
         }",
        "f",
    );
    assert!(broken.contains("(repeat"), "{broken}");
    assert_eq!(broken, guarded);
}

/// A loop with another way out is not just its first test.
#[test]
fn a_loop_with_a_second_break_keeps_its_shape() {
    let form = behavior_of(
        "fn f(n: usize, m: usize) -> usize {
             let mut i = 0;
             loop { if i >= n { break; } if i == m { break; } i += 1; }
             i
         }",
        "f",
    );
    assert!(form.contains("(const true)"), "{form}");
}

/// A repetition describes work, so a library that loops has a behavior.
#[test]
fn a_repetition_is_comparable() {
    let form = behavior_of(
        "fn f(values: &mut Vec<i32>) -> i32 {
             let mut total = 0;
             while let Some(value) = values.pop() { total += value; }
             total
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A counter loop is the traversal it is written to be.
///
/// Two laws compose to get here: the counter loop becomes a walk over a span,
/// and a span walk that only ever indexes becomes a walk over the elements.
#[test]
fn a_counter_loop_agrees_with_a_for_loop() {
    let counted = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             let mut i = 0;
             while i < values.len() { total += values[i]; i += 1; }
             total
         }",
        "f",
    );
    let direct = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             for i in 0..values.len() { total += values[i]; }
             total
         }",
        "f",
    );
    assert_eq!(counted, direct);
    assert!(!counted.contains("(repeat"), "{counted}");
}

/// A `loop` with a leading `break` reaches the same place.
///
/// Four laws in a row: the break becomes a guard, the guard's negation is read
/// as the counting test, the counter loop becomes a span walk, and the span
/// walk becomes an element walk.
#[test]
fn a_loop_with_a_break_agrees_with_a_for_loop() {
    let broken = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             let mut i = 0;
             loop { if i >= values.len() { break; } total += values[i]; i += 1; }
             total
         }",
        "f",
    );
    let direct = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             for i in 0..values.len() { total += values[i]; }
             total
         }",
        "f",
    );
    assert_eq!(broken, direct);
}

/// Counting down agrees with walking a reversed range.
#[test]
fn a_descending_counter_agrees_with_a_reversed_range() {
    let counted = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             let mut i = values.len();
             while i > 0 { i -= 1; total += values[i]; }
             total
         }",
        "f",
    );
    let reversed = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             for i in (0..values.len()).rev() { total += values[i]; }
             total
         }",
        "f",
    );
    assert_eq!(counted, reversed);
    assert!(counted.contains("traverse-back"), "{counted}");
}

/// Walking backwards is not the same as walking forwards.
#[test]
fn a_reversed_walk_differs_from_a_forward_one() {
    let forward = behavior_of("fn f(v: &[i32]) { for x in v.iter() { g(x); } }", "f");
    let backward = behavior_of("fn f(v: &[i32]) { for x in v.iter().rev() { g(x); } }", "f");
    assert_ne!(forward, backward);
}

/// Where the step sits decides which positions the loop visits.
///
/// Counting down with the decrement LAST visits `n` through `1`, not `n - 1`
/// through `0`, so it is a different span and is refused rather than given the
/// span its sibling has.
#[test]
fn a_descending_loop_that_steps_last_is_refused() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             let mut i = values.len();
             while i > 0 { total += values[i - 1]; i -= 1; }
             total
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A loop whose body can move the limit is not a walk over a fixed span.
#[test]
fn a_loop_that_changes_its_limit_is_refused() {
    let form = behavior_of(
        "fn f(values: &mut Vec<i32>) -> usize {
             let mut i = 0;
             while i < values.len() { values.push(1); i += 1; }
             i
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A counter something reads afterwards is not one a traversal may consume.
#[test]
fn a_counter_read_after_the_loop_is_refused() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> usize {
             let mut total = 0;
             let mut i = 0;
             while i < values.len() { total += values[i]; i += 1; }
             i
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A step that might not happen is not a span.
#[test]
fn a_conditional_step_is_refused() {
    let form = behavior_of(
        "fn f(values: &[i32]) -> i32 {
             let mut total = 0;
             let mut i = 0;
             while i < values.len() { if values[i] > 0 { i += 1; } total += 1; }
             total
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A stride is not a span.
#[test]
fn a_stride_is_refused() {
    let form = behavior_of(
        "fn f(n: usize) -> usize {
             let mut i = 0;
             let mut total = 0;
             while i < n { total += i; i += 2; }
             total
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// Nested counter loops reach the same walk over pairs as nested `for` loops.
///
/// The inner loop's own bound is `values.len()` as well, so this only holds
/// because working the limit out again counts as reading it.
#[test]
fn nested_counter_loops_agree_with_nested_for_loops() {
    let counted = behavior_of(
        "fn f(values: &[i32]) -> bool {
             let mut i = 0;
             while i < values.len() {
                 let mut j = i + 1;
                 while j < values.len() {
                     if values[i] == values[j] { return false; }
                     j += 1;
                 }
                 i += 1;
             }
             true
         }",
        "f",
    );
    let direct = behavior_of(
        "fn f(values: &[i32]) -> bool {
             for i in 0..values.len() {
                 for j in i + 1..values.len() {
                     if values[i] == values[j] { return false; }
                 }
             }
             true
         }",
        "f",
    );
    assert_eq!(counted, direct);
    assert!(counted.contains("(pairwise"), "{counted}");
}

/// Draining a container agrees with walking it.
#[test]
fn a_drain_agrees_with_a_for_loop() {
    let drained = behavior_of(
        "fn f(queue: &mut VecDeque<i32>) -> i32 {
             let mut total = 0;
             while let Some(x) = queue.pop_front() { total += x; }
             total
         }",
        "f",
    );
    let direct = behavior_of(
        "fn f(queue: VecDeque<i32>) -> i32 {
             let mut total = 0;
             for x in queue { total += x; }
             total
         }",
        "f",
    );
    assert_eq!(drained, direct);
}

/// Pulling from an iterator by hand agrees with walking it.
#[test]
fn a_hand_written_next_loop_agrees_with_a_for_loop() {
    let pulled = behavior_of(
        "fn f(items: &mut Iter<i32>) -> i32 {
             let mut total = 0;
             while let Some(x) = items.next() { total += x; }
             total
         }",
        "f",
    );
    let direct = behavior_of(
        "fn f(items: Iter<i32>) -> i32 {
             let mut total = 0;
             for x in items { total += x; }
             total
         }",
        "f",
    );
    assert_eq!(pulled, direct);
}

/// Taking from the back is not taking from the front.
#[test]
fn draining_from_each_end_gives_different_walks() {
    let front = behavior_of(
        "fn f(q: &mut VecDeque<i32>) -> i32 {
             let mut t = 0; while let Some(x) = q.pop_front() { t += x; } t
         }",
        "f",
    );
    let back = behavior_of(
        "fn f(q: &mut VecDeque<i32>) -> i32 {
             let mut t = 0; while let Some(x) = q.pop_back() { t += x; } t
         }",
        "f",
    );
    assert_ne!(front, back);
    assert!(back.contains("traverse-back"), "{back}");
}

/// A worklist is not a drain.
///
/// Putting something back makes the loop visit elements the container did not
/// start with, and it is the commoner shape by two to one.
#[test]
fn a_worklist_is_not_a_drain() {
    let form = behavior_of(
        "fn f(q: &mut VecDeque<i32>) -> i32 {
             let mut t = 0;
             while let Some(x) = q.pop_front() { t += x; if x > 0 { q.push_back(x - 1); } }
             t
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A bare `pop` settles no order, so it is not read as a walk.
///
/// It is the last element of a `Vec` and the greatest of a `BinaryHeap`, and
/// nothing in the form says which. Measured across the corpus, 434 of the 931
/// files that drain with `pop` also use a `BinaryHeap`.
#[test]
fn a_bare_pop_is_not_a_drain() {
    let form = behavior_of(
        "fn f(v: &mut Vec<i32>) -> i32 {
             let mut t = 0; while let Some(x) = v.pop() { t += x; } t
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// Measuring the container mid-drain observes a state a walk never has.
#[test]
fn a_body_that_measures_the_container_is_not_a_drain() {
    let form = behavior_of(
        "fn f(q: &mut VecDeque<i32>) -> usize {
             let mut t = 0; while let Some(_x) = q.pop_front() { t += q.len(); } t
         }",
        "f",
    );
    assert!(form.contains("(repeat"), "{form}");
}

/// A `while let` binds a name, and the body uses that name.
///
/// Held as a condition it was normalized as an expression, so the name came out
/// a hole that matched anything rather than the binding the body reads.
#[test]
fn a_while_let_binds_rather_than_leaving_a_hole() {
    let form = behavior_of(
        "fn f(v: &mut Vec<i32>) -> i32 {
             let mut t = 0; while let Some(x) = v.pop() { t += x; } t
         }",
        "f",
    );
    assert!(form.contains("(Some v"), "{form}");
}
