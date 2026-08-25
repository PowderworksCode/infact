//! Normalizes Rust syntax into Infact's language-neutral behavior form.
//!
//! This module knows Rust and nothing else. It names no crate, no callable, and
//! no API: every rule here is about how Rust spells something, so one
//! implementation covers every library a repository depends on.
//!
//! The canonicalizations, in order of how much they matter:
//!
//! 1. **Iteration form.** `for p in e { .. }` and `e.for_each(|p| ..)` describe
//!    the same traversal, as do `map`/`filter`/`fold` and their loop spellings.
//! 2. **Identity adapters.** `.iter()`, `.into_iter()`, `.by_ref()` and friends
//!    return what they were given, so they carry no behavior.
//! 3. **Alpha-renaming.** Identifiers become roles by binding order.
//! 4. **Construction.** `T::anything(..)` is construction of a `T`.
//! 5. **Syntactic noise.** Turbofish, casts, borrows, dereferences, parentheses,
//!    `?`, and blocks that wrap a single expression.

use entl_tree_sitter::ParsedFile;
use infact_normalize::{Arm, Direction, Form, Pattern, Roles};
use tree_sitter::Node;

/// Methods that hand back the sequence they were given.
///
/// These change nothing about what is iterated, so they are noise wherever they
/// appear — including as the whole expression. `itertools::sorted` ends with
/// `v.into_iter()`, which is how a function returns a sequence rather than
/// something it does to one.
const SEQUENCE_ADAPTERS: &[&str] = &[
    "iter",
    "into_iter",
    "iter_mut",
    "by_ref",
    "cloned",
    "copied",
    "to_vec",
    "as_slice",
    "as_str",
];

/// Conversions that are noise only when feeding something else.
///
/// `v.clone().iter().filter(p)` filters the same sequence as `v.iter()`, so a
/// conversion in a receiver chain is spelling. A conversion that *is* the
/// expression is not: `e.into()` in `Err(e) => e.into()` is the whole content of
/// `Result::into_ok`, and erasing it made that function identical to `into_err`,
/// its opposite. Nine findings named one of those two at random.
const VALUE_CONVERSIONS: &[&str] = &["as_ref", "as_mut", "to_owned", "clone", "into"];

/// Whether a method may be peeled off a receiver chain.
fn is_receiver_noise(name: &str) -> bool {
    SEQUENCE_ADAPTERS.contains(&name) || VALUE_CONVERSIONS.contains(&name)
}

/// Methods that visit each element without producing a new sequence.
const TRAVERSAL_METHODS: &[&str] = &["for_each", "try_for_each"];

/// Methods that produce a new sequence by transforming each element.
const TRANSFORM_METHODS: &[&str] = &["map", "flat_map"];

/// Methods that produce a sequence from the elements that yield a value.
///
/// `filter_map` used to be counted as a transform, which said that mapping and
/// mapping-while-dropping are the same operation. They are not: `map` cannot
/// change how many elements come out.
const SIFT_METHODS: &[&str] = &["filter_map"];

/// Methods that produce a new sequence by testing each element.
const RETAIN_METHODS: &[&str] = &["filter", "take_while", "skip_while"];

/// Methods that reduce a sequence to one accumulated value.
const ACCUMULATE_METHODS: &[&str] = &["fold", "try_fold"];

/// Associated functions that build a container out of a sequence.
///
/// `Vec::from_iter(items)` gathers a sequence; `Vec::new()` does not. Both are
/// associated functions on a type, so without this distinction the sequence
/// would vanish into a bare construction. These names come from the language's
/// own conversion traits, not from any library.
const SEQUENCE_CONSTRUCTORS: &[&str] = &["from_iter", "from", "collect"];

/// A named function found in a parsed file, with the body to normalize.
#[derive(Debug, Clone)]
pub struct NormalizedFunction {
    pub name: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
    pub form: Form,
    /// Where the body's braces are, so what the form was lifted from can be
    /// replaced by something lowered back out of it.
    pub body_start_byte: u64,
    pub body_end_byte: u64,
    /// What each role in `form` was called in the source.
    ///
    /// Beside the form rather than in it: the form must not carry it or two
    /// implementations that behave alike would stop comparing.
    pub names: Vec<(Form, String)>,
    /// Where each top-level statement of the body is, in the same order as the
    /// steps of `form` when `form` is a sequence. This is what lets a match be
    /// reported against statements rather than the whole function.
    pub statements: Vec<StatementSpan>,
    /// Whether the body runs at compile time, where there is no allocator.
    ///
    /// Not part of the form: what a function computes is the same whether or
    /// not it is `const`. It is recorded beside it because a recommendation to
    /// reach for a collection is wrong here however right the match is, and a
    /// reader should not be shown a finding they must then reject.
    pub is_const: bool,
}

/// The source extent of one statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementSpan {
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default()).unwrap_or_default()
}

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The operator a unary expression applies, as written.
///
/// The grammar gives it no field name, so it is the first child and it is
/// anonymous.
fn unary_operator<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    (node.kind() == "unary_expression")
        .then(|| node.child(0))
        .flatten()
        .map(|operator| text(operator, source))
}

/// Whether a range reaches its final endpoint.
///
/// `0..n` and `0..=n` differ by exactly one element, so which was written is
/// behavior. The operator is an anonymous token between the endpoints.
fn range_is_inclusive(node: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| !child.is_named() && text(child, source) == "..=")
}

/// Strip wrappers that carry no behavior.
///
/// A dereference is noise: `*x` and `x` denote the same value, and Rust writes
/// the star wherever a reference needs opening. Negation is NOT noise, and used
/// to be stripped here alongside it because both are spelled as a
/// `unary_expression`. That made `!seen.insert(x)` and `seen.insert(x)` — a
/// test and its opposite — the same form. So the choice is made on the
/// operator, not on the node kind.
fn unwrap_noise<'a>(mut node: Node<'a>, source: &'a [u8]) -> Node<'a> {
    loop {
        let inner = match node.kind() {
            "unary_expression" if unary_operator(node, source) == Some("*") => {
                named_children(node).into_iter().next()
            }
            "parenthesized_expression"
            | "reference_expression"
            // `?` is not noise: it is a conditional return, and a loop whose
            // test can leave the function does not do what a library that takes
            // a predicate does. Keeping it in the form is what lets a match
            // refuse it.
            | "await_expression"
            // an `else` is punctuation around the branch it introduces
            | "else_clause" => named_children(node).into_iter().next(),
            "type_cast_expression" => node.child_by_field_name("value"),
            _ => None,
        };
        match inner {
            Some(child) if !matches!(child.kind(), "type_identifier" | "primitive_type") => {
                node = child;
            }
            _ => return node,
        }
    }
}

/// Render a path with every `::<..>` removed, so `HashMap::<K, V>::new` and
/// `HashMap::new` are one path.
/// A pattern's parts, including the discarded ones.
///
/// Punctuation is skipped; everything else is a position, whether or not the
/// grammar gives it a name.
fn all_pattern_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "(" | ")" | "[" | "]" | "," | "|"))
        .collect()
}

/// Whether a name in pattern position names an alternative rather than binding.
fn is_variant_name(node: Node<'_>, source: &[u8]) -> bool {
    variant_name(node, source).starts_with(|first: char| first.is_ascii_uppercase())
}

/// The alternative a pattern's head names, without its path or generics.
fn variant_name(node: Node<'_>, source: &[u8]) -> String {
    let text = text(node, source);
    let bare = text.split_once('<').map_or(text, |(head, _)| head);
    bare.rsplit("::").next().unwrap_or(bare).trim().to_owned()
}

fn path_without_generics(node: Node<'_>, source: &[u8]) -> String {
    let raw = text(node, source);
    let mut output = String::new();
    let mut depth = 0usize;
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                if characters.peek() == Some(&':') {
                    characters.next();
                    if characters.peek() == Some(&'<') {
                        continue;
                    }
                    output.push_str("::");
                }
            }
            _ if depth == 0 => output.push(character),
            _ => {}
        }
    }
    output
}

fn starts_uppercase(segment: &str) -> bool {
    segment
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_uppercase())
}

/// The type a path constructs, if it looks like an associated function on a type.
fn constructed_type(path: &str) -> Option<&str> {
    let segments = path.split("::").collect::<Vec<_>>();
    if segments.len() < 2 {
        return None;
    }
    segments
        .iter()
        .rev()
        .skip(1)
        .find(|segment| starts_uppercase(segment))
        .copied()
}

/// A method call, split into its parts.
struct MethodCall<'a> {
    name: &'a str,
    receiver: Node<'a>,
    arguments: Vec<Node<'a>>,
}

/// The called function, with any turbofish wrapper removed.
///
/// `values.collect::<Vec<_>>()` parses as a `generic_function` around the
/// method, so the method is only visible after unwrapping.
fn called_function<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let function = node.child_by_field_name("function")?;
    if function.kind() == "generic_function" {
        return function.child_by_field_name("function").or(Some(function));
    }
    Some(function)
}

fn as_method_call<'a>(node: Node<'a>, source: &'a [u8]) -> Option<MethodCall<'a>> {
    if node.kind() != "call_expression" {
        return None;
    }
    let function = called_function(node)?;
    if function.kind() != "field_expression" {
        return None;
    }
    let name = text(function.child_by_field_name("field")?, source);
    let receiver = function.child_by_field_name("value")?;
    let arguments = node
        .child_by_field_name("arguments")
        .map(named_children)
        .unwrap_or_default();
    Some(MethodCall {
        name,
        receiver,
        arguments,
    })
}

/// Peel identity adapters off a receiver chain.
fn peel_adapters<'a>(mut node: Node<'a>, source: &'a [u8]) -> Node<'a> {
    loop {
        node = unwrap_noise(node, source);
        match as_method_call(node, source) {
            Some(call) if is_receiver_noise(call.name) && call.arguments.is_empty() => {
                node = call.receiver;
            }
            _ => return node,
        }
    }
}

/// The closure passed to a sequence method, as (parameters, body).
fn closure_parts<'a>(argument: Node<'a>, source: &'a [u8]) -> Option<(Vec<Node<'a>>, Node<'a>)> {
    let closure = unwrap_noise(argument, source);
    if closure.kind() != "closure_expression" {
        return None;
    }
    let parameters = closure
        .child_by_field_name("parameters")
        .map(named_children)
        .unwrap_or_default();
    let body = closure.child_by_field_name("body")?;
    Some((parameters, body))
}

struct Normalizer<'a> {
    source: &'a [u8],
    roles: Roles,
}

impl<'a> Normalizer<'a> {
    fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            roles: Roles::new(),
        }
    }

    fn bind_pattern(&mut self, node: Node<'_>) -> Pattern {
        match node.kind() {
            // A capitalized name in pattern position is a unit variant rather
            // than a binding: `None` names an alternative, `count` introduces
            // one. The language's own convention is what distinguishes them.
            "identifier" | "scoped_identifier" if is_variant_name(node, self.source) => {
                Pattern::Variant {
                    name: variant_name(node, self.source),
                    parts: Vec::new(),
                }
            }
            "identifier" => match self.roles.bind(text(node, self.source)) {
                Form::Local(index) => Pattern::Binding(index),
                _ => Pattern::Ignored,
            },
            "mut_pattern" | "ref_pattern" | "reference_pattern" => named_children(node)
                .into_iter()
                .next()
                .map_or(Pattern::Ignored, |child| self.bind_pattern(child)),
            // `Ok(value)` names the alternative it takes apart, and dropping
            // that name would make it indistinguishable from `Err(error)`.
            "tuple_struct_pattern" | "struct_pattern" => {
                let mut children = named_children(node).into_iter();
                let Some(head) = children.next() else {
                    return Pattern::Ignored;
                };
                Pattern::Variant {
                    name: variant_name(head, self.source),
                    parts: children.map(|child| self.bind_pattern(child)).collect(),
                }
            }
            // A discarded element is still an element. `_` is anonymous in the
            // grammar, so collecting only the named children dropped it and made
            // `(key, _)` and `(_, value)` the same pattern — which is why
            // `BTreeMap::keys` and `BTreeMap::values` derived to one form.
            "tuple_pattern" | "slice_pattern" => Pattern::Tuple(
                all_pattern_children(node)
                    .into_iter()
                    .map(|child| self.bind_pattern(child))
                    .collect(),
            ),
            _ => Pattern::Ignored,
        }
    }

    /// Recognize a sequence operation in any of its spellings.
    fn sequence_operation(&mut self, node: Node<'a>) -> Option<Form> {
        // `for p in e { body }`
        if node.kind() == "for_expression" {
            let pattern = node.child_by_field_name("pattern")?;
            let sequence = node.child_by_field_name("value")?;
            let body = node.child_by_field_name("body")?;
            let sequence = self.expression(peel_adapters(sequence, self.source));
            let item = self.bind_pattern(pattern);
            let body = self.expression(body);
            return Some(Form::Traverse {
                direction: Direction::Forward,
                sequence: Box::new(sequence),
                item: Box::new(item),
                body: Box::new(body),
            });
        }

        let call = as_method_call(node, self.source)?;

        // `e.collect()` / `e.collect::<Vec<_>>()`. A turbofish names the
        // container; without one the type is inferred and unavailable to us,
        // which is why the container is optional.
        if call.name == "collect" {
            let sequence = self.expression(peel_adapters(call.receiver, self.source));
            let container = node
                .child_by_field_name("function")
                .map(|function| text(function, self.source))
                .and_then(|raw| raw.split_once("::<").map(|(_, generics)| generics))
                .and_then(|generics| {
                    generics
                        .split(|character: char| !character.is_alphanumeric())
                        .find(|segment| starts_uppercase(segment))
                        .map(str::to_owned)
                });
            return Some(Form::Collect {
                sequence: Box::new(sequence),
                container,
            });
        }

        // `e.fold(init, |acc, item| body)`
        if ACCUMULATE_METHODS.contains(&call.name) && call.arguments.len() == 2 {
            let sequence = self.expression(peel_adapters(call.receiver, self.source));
            let initial = self.expression(call.arguments[0]);
            let (parameters, body) = closure_parts(call.arguments[1], self.source)?;
            let accumulator = parameters
                .first()
                .map_or(Pattern::Ignored, |node| self.bind_pattern(*node));
            let item = parameters
                .get(1)
                .map_or(Pattern::Ignored, |node| self.bind_pattern(*node));
            let body = self.expression(body);
            return Some(Form::Accumulate {
                sequence: Box::new(sequence),
                initial: Box::new(initial),
                accumulator: Box::new(accumulator),
                item: Box::new(item),
                body: Box::new(body),
            });
        }

        // `e.for_each(|p| body)`, `e.map(|p| body)`, `e.filter(|p| body)`
        let single_closure = |arguments: &[Node<'a>]| -> Option<(Vec<Node<'a>>, Node<'a>)> {
            if arguments.len() != 1 {
                return None;
            }
            closure_parts(arguments[0], self.source)
        };
        let kind = if TRAVERSAL_METHODS.contains(&call.name) {
            0
        } else if TRANSFORM_METHODS.contains(&call.name) {
            1
        } else if RETAIN_METHODS.contains(&call.name) {
            2
        } else if SIFT_METHODS.contains(&call.name) {
            3
        } else {
            return None;
        };
        if call.arguments.len() != 1 {
            return None;
        }
        let sequence = self.expression(peel_adapters(call.receiver, self.source));
        let (item, body) = match single_closure(&call.arguments) {
            Some((parameters, body)) => {
                let item = parameters
                    .first()
                    .map_or(Pattern::Ignored, |node| self.bind_pattern(*node));
                (item, self.expression(body))
            }
            // `map(f)` applies `f` to each item exactly as `map(|item| f(item))`
            // does. Naming the item makes the two spellings one form.
            None => {
                let function = self.expression(call.arguments[0]);
                let bound = self.roles.bind_anonymous();
                let item = match bound {
                    Form::Local(index) => Pattern::Binding(index),
                    _ => Pattern::Ignored,
                };
                (
                    item,
                    Form::Call {
                        callee: Box::new(function),
                        arguments: vec![bound],
                    },
                )
            }
        };
        let (sequence, item, body) = (Box::new(sequence), Box::new(item), Box::new(body));
        Some(match kind {
            0 => Form::Traverse {
                direction: Direction::Forward,
                sequence,
                item,
                body,
            },
            3 => Form::Sift {
                sequence,
                item,
                body,
            },
            1 => Form::Transform {
                sequence,
                item,
                body,
            },
            _ => Form::Retain {
                sequence,
                item,
                body,
            },
        })
    }

    fn expression(&mut self, node: Node<'a>) -> Form {
        let node = unwrap_noise(node, self.source);
        if let Some(form) = self.sequence_operation(node) {
            return form;
        }

        match node.kind() {
            "identifier" | "self" => {
                let name = text(node, self.source);
                // A bare capitalised name that nothing bound is a unit variant:
                // `None` is to `Some(x)` what `Continue` is to `Break(x)`, and
                // treating one as a value and the other as a variant would stop
                // them ever comparing.
                if starts_uppercase(name) && !self.roles.is_value(name) {
                    Form::Variant {
                        name: name.to_owned(),
                        payload: Vec::new(),
                    }
                } else {
                    self.roles.resolve(name)
                }
            }
            "scoped_identifier" | "generic_function" => {
                Form::Path(path_without_generics(node, self.source))
            }
            "field_expression" => {
                let value = node
                    .child_by_field_name("value")
                    .map_or(Form::Literal, |child| self.expression(child));
                let name = node
                    .child_by_field_name("field")
                    .map_or(String::new(), |child| text(child, self.source).to_owned());
                Form::Field {
                    value: Box::new(value),
                    name,
                }
            }
            "call_expression" => self.call(node),
            "assignment_expression" | "compound_assignment_expr" => {
                let target = node
                    .child_by_field_name("left")
                    .map_or(Form::Literal, |child| self.expression(child));
                let value = node
                    .child_by_field_name("right")
                    .map_or(Form::Literal, |child| self.expression(child));
                let operator = node
                    .child_by_field_name("operator")
                    .map_or("=", |child| text(child, self.source))
                    .to_owned();
                Form::Assign {
                    operator,
                    target: Box::new(target),
                    value: Box::new(value),
                }
            }
            "binary_expression" => {
                let left = node
                    .child_by_field_name("left")
                    .map_or(Form::Literal, |child| self.expression(child));
                let right = node
                    .child_by_field_name("right")
                    .map_or(Form::Literal, |child| self.expression(child));
                let operator = node
                    .child_by_field_name("operator")
                    .map_or("?", |child| text(child, self.source))
                    .to_owned();
                Form::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
            // A dereference has already been peeled by `unwrap_noise`, so
            // whatever reaches here applies an operator that changes the value.
            "unary_expression" => {
                let operator = unary_operator(node, self.source).unwrap_or_default();
                let value = named_children(node)
                    .into_iter()
                    .next()
                    .map_or(Form::Literal, |child| self.expression(child));
                Form::Unary {
                    operator: operator.to_owned(),
                    value: Box::new(value),
                }
            }
            // `s[i]`. The grammar names neither part, so they are positional:
            // the sequence first, then the position.
            "index_expression" => {
                let mut parts = named_children(node).into_iter();
                let (Some(sequence), Some(position)) = (parts.next(), parts.next()) else {
                    return Form::Literal;
                };
                Form::Index {
                    sequence: Box::new(self.expression(sequence)),
                    position: Box::new(self.expression(position)),
                }
            }
            // `a..b` and `a..=b`. An endpoint may be absent — `..b`, `a..`,
            // and bare `..` are all legal — and an absent one is not a bound
            // this can reason about, so it stays the syntax it was rather than
            // becoming a `Span` with an invented endpoint.
            "range_expression" => {
                let mut parts = named_children(node).into_iter();
                let (Some(start), Some(end)) = (parts.next(), parts.next()) else {
                    return Form::Opaque {
                        kind: "range_expression".to_owned(),
                        parts: named_children(node)
                            .into_iter()
                            .map(|child| self.expression(child))
                            .collect(),
                    };
                };
                let inclusive = range_is_inclusive(node, self.source);
                Form::Span {
                    start: Box::new(self.expression(start)),
                    end: Box::new(self.expression(end)),
                    inclusive,
                }
            }
            // a number's value can be the behavior, so it is kept
            "integer_literal" | "float_literal" => {
                Form::Number(text(node, self.source).replace('_', ""))
            }
            // What a library returns is behavior even when it is not a number:
            // `None => true` is a different decision from `None => ()`.
            "string_literal" | "raw_string_literal" | "boolean_literal" | "char_literal" => {
                Form::Constant(text(node, self.source).to_owned())
            }
            "unit_expression" => Form::Literal,
            // A struct literal builds a value of a named type, exactly as an
            // associated constructor does, so both reduce to construction.
            "struct_expression" => node
                .child_by_field_name("name")
                .map(|name| path_without_generics(name, self.source))
                .map_or(Form::Literal, |name| {
                    Form::Construct(name.rsplit("::").next().unwrap_or(name.as_str()).to_owned())
                }),
            "block" => self.block(node),
            "closure_expression" => {
                let parameters = node
                    .child_by_field_name("parameters")
                    .map(named_children)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|child| self.bind_pattern(child))
                    .collect();
                let body = node
                    .child_by_field_name("body")
                    .map_or(Form::Literal, |child| self.expression(child));
                Form::Lambda {
                    parameters,
                    body: Box::new(body),
                }
            }
            "if_expression" => {
                // `if let P = e { a } else { b }` decides the same thing as
                // `match e { P => a, _ => b }`, and only the second spelling was
                // being normalized. Left as syntax, the name the pattern binds
                // became a hole rather than a binding, so the two could never
                // compare and the binding matched anything.
                if let Some(condition) = node.child_by_field_name("condition")
                    && condition.kind() == "let_condition"
                    && let Some(selected) = self.select_from_let(node, condition)
                {
                    return selected;
                }
                let condition = node
                    .child_by_field_name("condition")
                    .map_or(Form::Literal, |child| self.expression(child));
                let consequence = node
                    .child_by_field_name("consequence")
                    .map_or(Form::Literal, |child| self.expression(child));
                let alternative = node
                    .child_by_field_name("alternative")
                    .map(|child| Box::new(self.expression(child)));
                Form::Branch {
                    condition: Box::new(condition),
                    consequence: Box::new(consequence),
                    alternative,
                }
            }
            // A macro's name is behavior: `panic!` and `format!` are not the
            // same thing, and leaving the name to become a role made every
            // macro call look like every other one.
            "macro_invocation" => {
                let name = node
                    .child_by_field_name("macro")
                    .map_or_else(String::new, |macro_name| {
                        variant_name(macro_name, self.source)
                    });
                Form::Opaque {
                    kind: format!("macro:{name}"),
                    parts: named_children(node)
                        .into_iter()
                        .filter(|child| child.kind() == "token_tree")
                        .map(|child| self.expression(child))
                        .collect(),
                }
            }
            "match_expression" => self.select(node),
            "return_expression" => {
                let value = named_children(node)
                    .into_iter()
                    .next()
                    .map_or(Form::Literal, |child| self.expression(child));
                Form::Return(Box::new(value))
            }
            kind => {
                let parts = named_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect();
                Form::Opaque {
                    kind: kind.to_owned(),
                    parts,
                }
            }
        }
    }

    /// `if let` as the decision it is.
    ///
    /// Returns `None` when the pattern names no alternative — `if let (a, b) = p`
    /// destructures rather than decides, and turning it into a one-armed
    /// decision would claim a choice that is not being made.
    fn select_from_let(&mut self, node: Node<'a>, condition: Node<'a>) -> Option<Form> {
        let scrutinee = self.expression(condition.child_by_field_name("value")?);
        let bound = self.bind_pattern(condition.child_by_field_name("pattern")?);
        if !matches!(bound, Pattern::Variant { .. }) {
            return None;
        }
        let taken = node
            .child_by_field_name("consequence")
            .map_or(Form::Literal, |child| self.expression(child));
        // An `if let` with no `else` yields nothing when the pattern does not
        // match, which is what the other arm has to say.
        let otherwise = node
            .child_by_field_name("alternative")
            .map_or(Form::Literal, |child| self.expression(child));
        Some(Form::select(
            scrutinee,
            vec![
                Arm {
                    pattern: bound,
                    body: taken,
                },
                Arm {
                    pattern: Pattern::Ignored,
                    body: otherwise,
                },
            ],
        ))
    }

    /// A `match`, as a decision among named alternatives.
    ///
    /// An arm with a guard is left as written: a guard makes the order of the
    /// arms part of what the code means, and a `Select` holds its arms sorted.
    fn select(&mut self, node: Node<'a>) -> Form {
        let Some(body) = node.child_by_field_name("body") else {
            return self.opaque(node);
        };
        let scrutinee = node
            .child_by_field_name("value")
            .map_or(Form::Literal, |child| self.expression(child));
        let mut arms = Vec::new();
        for arm in named_children(body) {
            if arm.kind() != "match_arm" {
                continue;
            }
            let Some(pattern) = arm.child_by_field_name("pattern") else {
                return self.opaque(node);
            };
            if pattern.child_by_field_name("condition").is_some() {
                return self.opaque(node);
            }
            let Some(bound) = named_children(pattern).into_iter().next() else {
                return self.opaque(node);
            };
            let bound = self.bind_pattern(bound);
            let value = arm
                .child_by_field_name("value")
                .map_or(Form::Literal, |child| self.expression(child));
            arms.push(Arm {
                pattern: bound,
                body: value,
            });
        }
        if arms.is_empty() {
            return self.opaque(node);
        }
        Form::select(scrutinee, arms)
    }

    fn opaque(&mut self, node: Node<'a>) -> Form {
        Form::Opaque {
            kind: node.kind().to_owned(),
            parts: named_children(node)
                .into_iter()
                .map(|child| self.expression(child))
                .collect(),
        }
    }

    fn call(&mut self, node: Node<'a>) -> Form {
        let Some(function) = called_function(node) else {
            return Form::Opaque {
                kind: "call_expression".to_owned(),
                parts: Vec::new(),
            };
        };

        // `T::whatever(..)` constructs a `T`. Which associated function was used
        // and what it received are not comparable across implementations, so
        // this is decided before the arguments are walked: normalizing them
        // would assign roles to names the canonical form goes on to discard.
        if matches!(function.kind(), "scoped_identifier" | "generic_function") {
            let path = path_without_generics(function, self.source);
            let leaf = path.rsplit("::").next().unwrap_or_default();
            // `ControlFlow::Break(x)` names a variant and carries a value;
            // `HashMap::with_capacity(8)` names a constructor and does not.
            // An uppercase final segment is the language's own signal for which.
            if starts_uppercase(leaf) {
                let payload = self.arguments(node);
                return Form::Variant {
                    name: path.clone(),
                    payload,
                };
            }
            if let Some(type_name) = constructed_type(&path) {
                let arguments = node
                    .child_by_field_name("arguments")
                    .map(named_children)
                    .unwrap_or_default();
                if SEQUENCE_CONSTRUCTORS.contains(&leaf) && arguments.len() == 1 {
                    let sequence = self.expression(peel_adapters(arguments[0], self.source));
                    return Form::Collect {
                        sequence: Box::new(sequence),
                        container: Some(type_name.to_owned()),
                    };
                }
                return Form::Construct(type_name.to_owned());
            }
            let arguments = self.arguments(node);
            return Form::Call {
                callee: Box::new(Form::Path(path)),
                arguments,
            };
        }

        // A bare name in call position is either a function defined elsewhere or
        // a value that happens to be callable. `helper(x)` names something the
        // reader could go and look at, and derivation follows it; `f(&item)`
        // calls whatever the caller supplied, so it has to stay a hole that any
        // argument, including an inline closure, can fill.
        if function.kind() == "identifier" {
            let name = text(function, self.source);
            if starts_uppercase(name) && !self.roles.is_value(name) {
                let payload = self.arguments(node);
                return Form::Variant {
                    name: name.to_owned(),
                    payload,
                };
            }
            let arguments = self.arguments(node);
            let callee = if self.roles.is_value(name) {
                self.roles.resolve(name)
            } else {
                Form::Path(name.to_owned())
            };
            return Form::Call {
                callee: Box::new(callee),
                arguments,
            };
        }

        if let Some(call) = as_method_call(node, self.source) {
            // A sequence adapter says nothing even standing alone; a value
            // conversion standing alone is the value being produced.
            if SEQUENCE_ADAPTERS.contains(&call.name) && call.arguments.is_empty() {
                let receiver = peel_adapters(call.receiver, self.source);
                return self.expression(receiver);
            }
            let name = call.name.to_owned();
            let receiver = self.expression(peel_adapters(call.receiver, self.source));
            let arguments = self.arguments(node);
            return Form::Method {
                name,
                receiver: Box::new(receiver),
                arguments,
            };
        }

        let callee = self.expression(function);
        let arguments = self.arguments(node);
        Form::Call {
            callee: Box::new(callee),
            arguments,
        }
    }

    fn arguments(&mut self, node: Node<'a>) -> Vec<Form> {
        node.child_by_field_name("arguments")
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .map(|argument| self.expression(argument))
            .collect()
    }

    fn block(&mut self, node: Node<'a>) -> Form {
        let mut steps = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "let_declaration" => {
                    // the value is normalized before the name is bound so a
                    // `let` cannot capture its own name
                    let value = child
                        .child_by_field_name("value")
                        .map_or(Form::Literal, |value| self.expression(value));
                    let pattern = child
                        .child_by_field_name("pattern")
                        .map_or(Pattern::Ignored, |pattern| self.bind_pattern(pattern));
                    steps.push(Form::Let {
                        pattern: Box::new(pattern),
                        value: Box::new(value),
                    });
                }
                "expression_statement" => {
                    if let Some(inner) = named_children(child).into_iter().next() {
                        steps.push(self.expression(inner));
                    }
                }
                // A function declared inside a body is a name bound to a body,
                // exactly like a `let` bound to a closure. Treating it as one
                // makes it something a later pass can unfold; treating it as
                // opaque syntax buries the body under its own type signature,
                // which is how `find` came to be thirty nodes of `abstract_type`.
                "function_item" => {
                    let parameters = child
                        .child_by_field_name("parameters")
                        .map(named_children)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|parameter| {
                            parameter
                                .child_by_field_name("pattern")
                                .map_or(Pattern::Ignored, |pattern| self.bind_pattern(pattern))
                        })
                        .collect();
                    let body = child
                        .child_by_field_name("body")
                        .map_or(Form::Literal, |body| self.block(body));
                    let name = child
                        .child_by_field_name("name")
                        .map_or(Pattern::Ignored, |name| self.bind_pattern(name));
                    steps.push(Form::Let {
                        pattern: Box::new(name),
                        value: Box::new(Form::Lambda {
                            parameters,
                            body: Box::new(body),
                        }),
                    });
                }
                "line_comment" | "block_comment" | "attribute_item" => {}
                _ => steps.push(self.expression(child)),
            }
        }
        // A block wrapping a single expression is that expression. Without this
        // a loop body, always a block, could never equal a closure body, usually
        // bare, which is the difference this pass exists to erase.
        match steps.as_slice() {
            [only] if !matches!(only, Form::Let { .. }) => only.clone(),
            _ => Form::Sequence(steps),
        }
    }
}

/// Every name a parameter list introduces, including a receiver.
fn parameter_names<'a>(node: Node<'a>, source: &'a [u8], names: &mut Vec<&'a str>) {
    match node.kind() {
        "identifier" => names.push(text(node, source)),
        "self_parameter" => names.push("self"),
        // a parameter's type is not a name the body can call
        "parameter" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                parameter_names(pattern, source, names);
            }
        }
        _ => {
            for child in named_children(node) {
                parameter_names(child, source, names);
            }
        }
    }
}

/// The source extent of each top-level statement in a body.
///
/// Kept in the order `block` walks them, so step *n* of a sequence form is
/// statement *n* here.
fn statement_spans(body: Node<'_>) -> Vec<StatementSpan> {
    named_children(body)
        .into_iter()
        .filter(|child| {
            !matches!(
                child.kind(),
                "line_comment" | "block_comment" | "attribute_item"
            )
        })
        .map(|child| StatementSpan {
            start_byte: child.start_byte() as u64,
            end_byte: child.end_byte() as u64,
            start_line: u32::try_from(child.start_position().row + 1).unwrap_or(u32::MAX),
            end_line: u32::try_from(child.end_position().row + 1).unwrap_or(u32::MAX),
        })
        .collect()
}

/// Normalize one function, declaring its parameters before its body.
///
/// Parameters are what let a call be read correctly: without them every bare
/// call looks alike, and a function the reader could go and look at cannot be
/// told apart from an argument the caller supplied.
pub fn normalize_function(function: Node<'_>, source: &[u8]) -> Form {
    normalize_function_named(function, source).0
}

/// Normalize one function, keeping what each role was called.
///
/// The names are returned separately rather than placed in the form. A form
/// that carried them would no longer compare two implementations that differ
/// only in what their authors named things, which is the whole point of it.
pub fn normalize_function_named(function: Node<'_>, source: &[u8]) -> (Form, Vec<(Form, String)>) {
    let mut normalizer = Normalizer::new(source);
    if let Some(parameters) = function.child_by_field_name("parameters") {
        let mut names = Vec::new();
        parameter_names(parameters, source, &mut names);
        for name in names {
            normalizer.roles.declare(name);
        }
    }
    normalizer.roles.declare("self");
    let form = match function.child_by_field_name("body") {
        Some(body) => normalizer.block(body),
        None => Form::Sequence(Vec::new()),
    };
    let names = normalizer.roles.ledger().to_vec();
    (form, names)
}

/// Normalize a bare body, with no parameters in scope.
pub fn normalize_body(body: Node<'_>, source: &[u8]) -> Form {
    let mut normalizer = Normalizer::new(source);
    normalizer.block(body)
}

/// Whether a function is declared `const`.
///
/// The modifier is an anonymous token before the `fn`, so it is read off the
/// children rather than a field.
fn is_const_function(node: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .take_while(|child| child.kind() != "fn")
        .any(|child| !child.is_named() && text(child, source) == "const")
}

fn collect_functions<'a>(node: Node<'a>, output: &mut Vec<Node<'a>>) {
    if node.kind() == "function_item" {
        output.push(node);
    }
    for child in named_children(node) {
        collect_functions(child, output);
    }
}

/// Normalize every function in a parsed Rust file.
pub fn normalize_file(file: &ParsedFile) -> Vec<NormalizedFunction> {
    let mut nodes = Vec::new();
    collect_functions(file.tree.root_node(), &mut nodes);
    nodes
        .into_iter()
        .filter_map(|node| {
            let name = node.child_by_field_name("name")?;
            // a signature with no body declares behavior rather than describing it
            let body = node.child_by_field_name("body")?;
            let (form, names) = normalize_function_named(node, &file.source);
            Some(NormalizedFunction {
                body_start_byte: body.start_byte() as u64,
                body_end_byte: body.end_byte() as u64,
                names,
                name: text(name, &file.source).to_owned(),
                start_byte: node.start_byte() as u64,
                end_byte: node.end_byte() as u64,
                start_line: u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX),
                end_line: u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX),
                form,
                statements: node
                    .child_by_field_name("body")
                    .map(statement_spans)
                    .unwrap_or_default(),
                is_const: is_const_function(node, &file.source),
            })
        })
        .collect()
}
