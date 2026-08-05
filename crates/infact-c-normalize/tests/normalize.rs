//! The normalizations that let C meet the behavior form.
//!
//! Each fixture exercises one spelling: the index walk that is C's only
//! native iteration, the pointer ceremony that reduces to what it wraps, and
//! the control shapes with direct equivalents. What has no canonical shape
//! stays opaque, and one test pins that too.

use std::path::PathBuf;

use entl_tree_sitter::{ParsedFile, ParserCatalog, ParserRuntime};
use infact_c_normalize::normalize_file;
use infact_normalize::{Direction, Form};

fn parse(name: &str, source: &str) -> ParsedFile {
    let discovery = ParserCatalog::discover([
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../entl/parser-packs")
    ]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    let path = PathBuf::from(name);
    let pack = discovery
        .catalog
        .resolve("c", &path)
        .expect("no C parser pack")
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
    let file = parse("fixture.c", source);
    normalize_file(&file)
        .into_iter()
        .find(|candidate| candidate.name == function)
        .unwrap_or_else(|| panic!("no function {function}"))
        .form
}

#[test]
fn index_walk_is_a_traversal() {
    let form = form(
        "int sum(int *a, int n) {\n\
         \tint total = 0;\n\
         \tfor (int i = 0; i < n; i++)\n\
         \t\ttotal += a[i];\n\
         \treturn total;\n\
         }\n",
        "sum",
    );
    let Form::Sequence(steps) = &form else {
        panic!("expected a sequence, got {form:?}");
    };
    let Form::Traverse {
        body, direction, ..
    } = &steps[1]
    else {
        panic!("expected a traversal, got {:?}", steps[1]);
    };
    assert_eq!(*direction, Direction::Forward);
    // `total += a[i]` with the element access replaced by the item.
    let Form::Assign {
        operator, value, ..
    } = body.as_ref()
    else {
        panic!("expected an accumulation body, got {body:?}");
    };
    assert_eq!(operator, "+=");
    assert!(matches!(value.as_ref(), Form::Local(_)));
}

#[test]
fn leading_element_declaration_names_the_item() {
    let form = form(
        "void each(struct entry *entries, int count) {\n\
         \tfor (int i = 0; i < count; i++) {\n\
         \t\tstruct entry e = entries[i];\n\
         \t\tuse(e);\n\
         \t}\n\
         }\n",
        "each",
    );
    let Form::Traverse { body, .. } = &form else {
        panic!("expected a traversal, got {form:?}");
    };
    // The body is `use(item)`: the declaration was consumed by the binding.
    let Form::Call { arguments, .. } = body.as_ref() else {
        panic!("expected a call body, got {body:?}");
    };
    assert!(matches!(arguments[0], Form::Local(_)));
}

#[test]
fn backwards_walk_carries_its_direction() {
    let form = form(
        "void drain(int *a, int n) {\n\
         \tfor (int i = n - 1; i >= 0; i--)\n\
         \t\ttake(a[i]);\n\
         }\n",
        "drain",
    );
    let Form::Traverse { direction, .. } = &form else {
        panic!("expected a traversal, got {form:?}");
    };
    assert_eq!(*direction, Direction::Backward);
}

#[test]
fn pointer_ceremony_reduces_to_what_it_wraps() {
    let form = form("int deref(int *p) { return *p; }\n", "deref");
    // `return *p` is `return p` once the ceremony is gone.
    assert!(matches!(form, Form::Return(ref inner)
        if matches!(inner.as_ref(), Form::Free(0))));
}

#[test]
fn casts_are_annotation_not_behavior() {
    let form = form("long widen(int x) { return (long)x; }\n", "widen");
    assert!(matches!(form, Form::Return(ref inner)
        if matches!(inner.as_ref(), Form::Free(0))));
}

#[test]
fn a_switch_is_a_select_with_named_arms() {
    let form = form(
        "int pick(int kind) {\n\
         \tswitch (kind) {\n\
         \tcase 1: return 10;\n\
         \tcase 2: return 20;\n\
         \t}\n\
         \treturn 0;\n\
         }\n",
        "pick",
    );
    let Form::Sequence(steps) = &form else {
        panic!("expected a sequence, got {form:?}");
    };
    let Form::Select { arms, .. } = &steps[0] else {
        panic!("expected a select, got {:?}", steps[0]);
    };
    assert_eq!(arms.len(), 2);
}

#[test]
fn increment_is_accumulation() {
    let form = form("void bump(int x) { x++; }\n", "bump");
    let Form::Assign {
        operator, value, ..
    } = &form
    else {
        panic!("expected an assignment, got {form:?}");
    };
    assert_eq!(operator, "+=");
    assert_eq!(*value.as_ref(), Form::Number("1".to_owned()));
}

#[test]
fn goto_stays_opaque() {
    let form = form(
        "void jump(int x) {\n\
         \tif (x)\n\
         \t\tgoto out;\n\
         out:\n\
         \treturn;\n\
         }\n",
        "jump",
    );
    let Form::Sequence(steps) = &form else {
        panic!("expected a sequence, got {form:?}");
    };
    let Form::Branch { consequence, .. } = &steps[0] else {
        panic!("expected a branch, got {:?}", steps[0]);
    };
    assert!(matches!(consequence.as_ref(), Form::Opaque { kind, .. } if kind == "goto"));
}

#[test]
fn null_is_a_constant_not_a_name() {
    let form = form("int check(char *p) { return p == NULL; }\n", "check");
    let Form::Return(inner) = &form else {
        panic!("expected a return, got {form:?}");
    };
    let Form::Binary { right, .. } = inner.as_ref() else {
        panic!("expected a comparison, got {inner:?}");
    };
    assert_eq!(*right.as_ref(), Form::Constant("NULL".to_owned()));
}

#[test]
fn dialect_rewritten_iterator_macros_normalize_too() {
    // `for_each_string_list_item` reaches this crate as `if(...)` after the
    // dialect rewrite; the file parses and the body still normalizes. This
    // pins the contract between the pack's rewrites and this normalizer.
    let file = parse(
        "fixture.c",
        "void walk(struct string_list *list) {\n\
         \tfor_each_string_list_item(item, list) {\n\
         \t\tuse(item);\n\
         \t}\n\
         }\n",
    );
    let functions = normalize_file(&file);
    assert_eq!(functions.len(), 1);
    assert!(!file.rewrites.is_empty(), "the dialect rewrite should fire");
}
