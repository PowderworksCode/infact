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
