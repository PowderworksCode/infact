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
use infact_normalize::{Form, Pattern, Roles};
use tree_sitter::Node;

/// Methods that return the sequence they were given.
const IDENTITY_ADAPTERS: &[&str] = &[
    "iter",
    "into_iter",
    "iter_mut",
    "by_ref",
    "as_ref",
    "as_mut",
    "as_slice",
    "as_str",
    "to_owned",
    "to_vec",
    "clone",
    "cloned",
    "copied",
    "into",
];

/// Methods that visit each element without producing a new sequence.
const TRAVERSAL_METHODS: &[&str] = &["for_each", "try_for_each"];

/// Methods that produce a new sequence by transforming each element.
const TRANSFORM_METHODS: &[&str] = &["map", "filter_map", "flat_map"];

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
    /// Where each top-level statement of the body is, in the same order as the
    /// steps of `form` when `form` is a sequence. This is what lets a match be
    /// reported against statements rather than the whole function.
    pub statements: Vec<StatementSpan>,
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

/// Strip wrappers that carry no behavior.
fn unwrap_noise(mut node: Node<'_>) -> Node<'_> {
    loop {
        let inner = match node.kind() {
            "parenthesized_expression"
            | "unary_expression"
            | "reference_expression"
            | "try_expression"
            | "await_expression" => named_children(node).into_iter().next(),
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
        node = unwrap_noise(node);
        match as_method_call(node, source) {
            Some(call) if IDENTITY_ADAPTERS.contains(&call.name) && call.arguments.is_empty() => {
                node = call.receiver;
            }
            _ => return node,
        }
    }
}

/// The closure passed to a sequence method, as (parameters, body).
fn closure_parts<'a>(argument: Node<'a>) -> Option<(Vec<Node<'a>>, Node<'a>)> {
    let closure = unwrap_noise(argument);
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
            "identifier" => match self.roles.bind(text(node, self.source)) {
                Form::Local(index) => Pattern::Binding(index),
                _ => Pattern::Ignored,
            },
            "mut_pattern" | "ref_pattern" | "reference_pattern" => named_children(node)
                .into_iter()
                .next()
                .map_or(Pattern::Ignored, |child| self.bind_pattern(child)),
            "tuple_pattern" | "tuple_struct_pattern" | "slice_pattern" => Pattern::Tuple(
                named_children(node)
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
            let (parameters, body) = closure_parts(call.arguments[1])?;
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
            closure_parts(arguments[0])
        };
        let kind = if TRAVERSAL_METHODS.contains(&call.name) {
            0
        } else if TRANSFORM_METHODS.contains(&call.name) {
            1
        } else if RETAIN_METHODS.contains(&call.name) {
            2
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
        let node = unwrap_noise(node);
        if let Some(form) = self.sequence_operation(node) {
            return form;
        }

        match node.kind() {
            "identifier" | "self" => self.roles.resolve(text(node, self.source)),
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
            // a number's value can be the behavior, so it is kept
            "integer_literal" | "float_literal" => {
                Form::Number(text(node, self.source).replace('_', ""))
            }
            "string_literal" | "raw_string_literal" | "boolean_literal" | "char_literal"
            | "unit_expression" => Form::Literal,
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
            if let Some(type_name) = constructed_type(&path) {
                let leaf = path.rsplit("::").next().unwrap_or_default();
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
            if IDENTITY_ADAPTERS.contains(&call.name) && call.arguments.is_empty() {
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
    let mut normalizer = Normalizer::new(source);
    if let Some(parameters) = function.child_by_field_name("parameters") {
        let mut names = Vec::new();
        parameter_names(parameters, source, &mut names);
        for name in names {
            normalizer.roles.declare(name);
        }
    }
    normalizer.roles.declare("self");
    match function.child_by_field_name("body") {
        Some(body) => normalizer.block(body),
        None => Form::Sequence(Vec::new()),
    }
}

/// Normalize a bare body, with no parameters in scope.
pub fn normalize_body(body: Node<'_>, source: &[u8]) -> Form {
    let mut normalizer = Normalizer::new(source);
    normalizer.block(body)
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
            node.child_by_field_name("body")?;
            Some(NormalizedFunction {
                name: text(name, &file.source).to_owned(),
                start_byte: node.start_byte() as u64,
                end_byte: node.end_byte() as u64,
                start_line: u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX),
                end_line: u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX),
                form: normalize_function(node, &file.source),
                statements: node
                    .child_by_field_name("body")
                    .map(statement_spans)
                    .unwrap_or_default(),
            })
        })
        .collect()
}
