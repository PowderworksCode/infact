//! Rust source into the faithful tree.
//!
//! Every rule here either describes a node exactly or declines to. Declining
//! is [`Expr::Verbatim`] holding the node's own text, which prints back as
//! itself, so a gap in coverage costs reach rather than correctness. Nothing
//! here may guess: a rule that is unsure must decline, because a wrong
//! structure prints a different program while verbatim text never can.

use entl_tree_sitter::ParsedFile;
use tree_sitter::Node;

use crate::syntax::{
    Arm, Block, Capture, Condition, Delimiter, Expr, FieldInit, FieldPat, LiftedBody, Pat, Stmt,
};

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default()).unwrap_or_default()
}

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// Whether a token follows a node, which is how a missing semicolon is seen.
fn followed_by(node: Node<'_>, token: &str) -> bool {
    node.next_sibling()
        .is_some_and(|sibling| sibling.kind() == token)
}

pub struct Lifter<'a> {
    source: &'a [u8],
}

impl<'a> Lifter<'a> {
    #[must_use]
    pub fn new(source: &'a [u8]) -> Self {
        Self { source }
    }

    fn verbatim(&self, node: Node<'_>) -> Expr {
        Expr::Verbatim(text(node, self.source).to_owned())
    }

    /// The attributes written above a statement or an arm.
    fn attributes(&self, node: Node<'_>) -> Vec<String> {
        let mut found = Vec::new();
        let mut sibling = node.prev_sibling();
        while let Some(current) = sibling {
            if current.kind() == "attribute_item" {
                found.push(text(current, self.source).to_owned());
                sibling = current.prev_sibling();
            } else {
                break;
            }
        }
        found.reverse();
        found
    }

    pub fn block(&self, node: Node<'a>) -> Block {
        let mut statements = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "line_comment" | "block_comment" => {
                    let trailing = child.prev_sibling().is_some_and(|previous| {
                        previous.end_position().row == child.start_position().row
                    });
                    statements.push(Stmt::Comment {
                        text: text(child, self.source).to_owned(),
                        trailing,
                    });
                }
                // Attributes are collected by the statement they sit above.
                "attribute_item" => {}
                "let_declaration" => statements.push(self.let_statement(child)),
                "expression_statement" => {
                    let Some(inner) = named_children(child).into_iter().next() else {
                        continue;
                    };
                    // `expr;` and `if .. {}` both parse as expression
                    // statements; only the first has a semicolon, and whether
                    // one is printed decides whether the block has a value.
                    let semicolon = children(child).iter().any(|token| token.kind() == ";");
                    statements.push(Stmt::Expr {
                        attributes: self.attributes(child),
                        value: self.expression(inner),
                        semicolon,
                    });
                }
                "function_item"
                | "struct_item"
                | "enum_item"
                | "impl_item"
                | "use_declaration"
                | "const_item"
                | "static_item"
                | "type_item"
                | "mod_item"
                | "trait_item"
                | "macro_definition"
                | "union_item"
                | "extern_crate_declaration" => {
                    statements.push(Stmt::Item(text(child, self.source).to_owned()));
                }
                _ => {
                    // The tail expression of a block: no semicolon, so it is
                    // the block's value.
                    statements.push(Stmt::Expr {
                        attributes: self.attributes(child),
                        value: self.expression(child),
                        semicolon: followed_by(child, ";"),
                    });
                }
            }
        }
        Block { statements }
    }

    fn let_statement(&self, node: Node<'a>) -> Stmt {
        // `let mut x` puts the `mut` beside the pattern rather than around it,
        // so the binding does not carry it and asking the pattern alone loses
        // every `let mut` in the file.
        let mutable = children(node)
            .iter()
            .any(|child| child.kind() == "mutable_specifier");
        let pattern = node
            .child_by_field_name("pattern")
            .map_or(Pat::Wild, |child| self.pattern(child));
        let pattern = match (mutable, pattern) {
            (
                true,
                Pat::Binding {
                    by_reference,
                    name,
                    subpattern,
                    ..
                },
            ) => Pat::Binding {
                by_reference,
                mutable: true,
                name,
                subpattern,
            },
            (_, pattern) => pattern,
        };
        let annotation = node
            .child_by_field_name("type")
            .map(|child| text(child, self.source).to_owned());
        let value = node
            .child_by_field_name("value")
            .map(|child| self.expression(child));
        // `let .. else { .. }` — the block is the last child and is not the
        // value.
        let diverging = node
            .child_by_field_name("alternative")
            .map(|child| self.block(child));
        Stmt::Let {
            attributes: self.attributes(node),
            pattern,
            annotation,
            value,
            diverging,
        }
    }

    fn label(&self, node: Node<'_>) -> Option<String> {
        named_children(node)
            .into_iter()
            .find(|child| child.kind() == "label")
            .map(|child| text(child, self.source).to_owned())
    }

    fn condition(&self, node: Node<'a>) -> Condition {
        match node.kind() {
            "let_condition" => {
                let pattern = node
                    .child_by_field_name("pattern")
                    .map_or(Pat::Wild, |child| self.pattern(child));
                let value = node
                    .child_by_field_name("value")
                    .map_or(Expr::Unit, |child| self.expression(child));
                Condition::Let { pattern, value }
            }
            "let_chain" => Condition::Chain(
                named_children(node)
                    .into_iter()
                    .map(|child| self.condition(child))
                    .collect(),
            ),
            _ => Condition::Plain(self.expression(node)),
        }
    }

    fn arguments(&self, node: Node<'a>) -> Vec<Expr> {
        node.child_by_field_name("arguments")
            .map(named_children)
            .unwrap_or_default()
            .into_iter()
            .map(|child| self.expression(child))
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    pub fn expression(&self, node: Node<'a>) -> Expr {
        match node.kind() {
            "identifier" | "scoped_identifier" | "self" | "crate" | "super" | "metavariable" => {
                Expr::Path(text(node, self.source).to_owned())
            }
            // `foo::<T>` used as a value.
            "generic_function" => Expr::Path(text(node, self.source).to_owned()),
            "integer_literal" | "float_literal" | "string_literal" | "raw_string_literal"
            | "boolean_literal" | "char_literal" | "negative_literal" => {
                Expr::Literal(text(node, self.source).to_owned())
            }
            "unit_expression" => Expr::Unit,
            "field_expression" => {
                let Some(value) = node.child_by_field_name("value") else {
                    return self.verbatim(node);
                };
                let Some(field) = node.child_by_field_name("field") else {
                    return self.verbatim(node);
                };
                Expr::Field {
                    value: Box::new(self.expression(value)),
                    name: text(field, self.source).to_owned(),
                }
            }
            "call_expression" => self.call(node),
            "binary_expression" => {
                let (Some(left), Some(right), Some(operator)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                    node.child_by_field_name("operator"),
                ) else {
                    return self.verbatim(node);
                };
                Expr::Binary {
                    left: Box::new(self.expression(left)),
                    operator: text(operator, self.source).to_owned(),
                    right: Box::new(self.expression(right)),
                }
            }
            // `-x`, `!x`, `*x`. The grammar gives the operator no field, so it
            // is the leading token.
            "unary_expression" => {
                let Some(operand) = named_children(node).into_iter().next() else {
                    return self.verbatim(node);
                };
                let Some(operator) = children(node).into_iter().find(|child| !child.is_named())
                else {
                    return self.verbatim(node);
                };
                Expr::Unary {
                    operator: text(operator, self.source).to_owned(),
                    operand: Box::new(self.expression(operand)),
                }
            }
            "reference_expression" => {
                let Some(value) = node.child_by_field_name("value") else {
                    return self.verbatim(node);
                };
                Expr::Reference {
                    mutable: children(node)
                        .iter()
                        .any(|c| c.kind() == "mutable_specifier"),
                    value: Box::new(self.expression(value)),
                }
            }
            "assignment_expression" | "compound_assignment_expr" => {
                let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) else {
                    return self.verbatim(node);
                };
                let operator = node
                    .child_by_field_name("operator")
                    .map_or("=", |child| text(child, self.source))
                    .to_owned();
                Expr::Assign {
                    target: Box::new(self.expression(left)),
                    operator,
                    value: Box::new(self.expression(right)),
                }
            }
            "type_cast_expression" => {
                let (Some(value), Some(annotation)) = (
                    node.child_by_field_name("value"),
                    node.child_by_field_name("type"),
                ) else {
                    return self.verbatim(node);
                };
                Expr::Cast {
                    value: Box::new(self.expression(value)),
                    annotation: text(annotation, self.source).to_owned(),
                }
            }
            "try_expression" => named_children(node).into_iter().next().map_or_else(
                || self.verbatim(node),
                |child| Expr::Try(Box::new(self.expression(child))),
            ),
            "await_expression" => named_children(node).into_iter().next().map_or_else(
                || self.verbatim(node),
                |child| Expr::Await(Box::new(self.expression(child))),
            ),
            "parenthesized_expression" => named_children(node).into_iter().next().map_or_else(
                || self.verbatim(node),
                |child| Expr::Parenthesized(Box::new(self.expression(child))),
            ),
            "closure_expression" => {
                let Some(body) = node.child_by_field_name("body") else {
                    return self.verbatim(node);
                };
                // `|_|` again: the discarded parameter is anonymous, so asking
                // for the named children turns it into `||` and the closure
                // stops taking an argument.
                let parameters = node
                    .child_by_field_name("parameters")
                    .map(|list| self.pattern_parts(list))
                    .unwrap_or_default();
                let tokens = children(node);
                Expr::Closure {
                    capture: if tokens.iter().any(|child| child.kind() == "move") {
                        Capture::ByValue
                    } else {
                        Capture::ByReference
                    },
                    asynchronous: tokens.iter().any(|child| child.kind() == "async"),
                    parameters,
                    annotation: node
                        .child_by_field_name("return_type")
                        .map(|child| text(child, self.source).to_owned()),
                    body: Box::new(self.expression(body)),
                }
            }
            "if_expression" => {
                let (Some(condition), Some(consequence)) = (
                    node.child_by_field_name("condition"),
                    node.child_by_field_name("consequence"),
                ) else {
                    return self.verbatim(node);
                };
                let alternative = node.child_by_field_name("alternative").and_then(|clause| {
                    // `else_clause` wraps the block or the next `if`.
                    let inner = if clause.kind() == "else_clause" {
                        named_children(clause).into_iter().next()?
                    } else {
                        clause
                    };
                    Some(Box::new(self.expression(inner)))
                });
                Expr::If {
                    condition: Box::new(self.condition(condition)),
                    consequence: self.block(consequence),
                    alternative,
                }
            }
            "match_expression" => self.match_expression(node),
            "while_expression" => {
                let (Some(condition), Some(body)) = (
                    node.child_by_field_name("condition"),
                    node.child_by_field_name("body"),
                ) else {
                    return self.verbatim(node);
                };
                Expr::While {
                    label: self.label(node),
                    condition: Box::new(self.condition(condition)),
                    body: self.block(body),
                }
            }
            "for_expression" => {
                let (Some(pattern), Some(sequence), Some(body)) = (
                    node.child_by_field_name("pattern"),
                    node.child_by_field_name("value"),
                    node.child_by_field_name("body"),
                ) else {
                    return self.verbatim(node);
                };
                Expr::For {
                    label: self.label(node),
                    pattern: self.pattern(pattern),
                    sequence: Box::new(self.expression(sequence)),
                    body: self.block(body),
                }
            }
            "loop_expression" => node.child_by_field_name("body").map_or_else(
                || self.verbatim(node),
                |body| Expr::Loop {
                    label: self.label(node),
                    body: self.block(body),
                },
            ),
            "block" => Expr::Block {
                label: None,
                modifiers: Vec::new(),
                body: self.block(node),
            },
            "unsafe_block" | "async_block" | "const_block" | "try_block" => {
                let Some(body) = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "block")
                else {
                    return self.verbatim(node);
                };
                let modifiers = children(node)
                    .into_iter()
                    .filter(|child| {
                        matches!(child.kind(), "unsafe" | "async" | "const" | "try" | "move")
                    })
                    .map(|child| text(child, self.source).to_owned())
                    .collect();
                Expr::Block {
                    label: None,
                    modifiers,
                    body: self.block(body),
                }
            }
            "labeled_block" | "labeled_expression" => {
                let Some(body) = named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "block")
                else {
                    return self.verbatim(node);
                };
                Expr::Block {
                    label: self.label(node),
                    modifiers: Vec::new(),
                    body: self.block(body),
                }
            }
            "return_expression" => Expr::Return(
                named_children(node)
                    .into_iter()
                    .next()
                    .map(|child| Box::new(self.expression(child))),
            ),
            "break_expression" => {
                let parts = named_children(node);
                Expr::Break {
                    label: parts
                        .iter()
                        .find(|child| child.kind() == "label")
                        .map(|child| text(*child, self.source).to_owned()),
                    value: parts
                        .iter()
                        .find(|child| child.kind() != "label")
                        .map(|child| Box::new(self.expression(*child))),
                }
            }
            "continue_expression" => Expr::Continue {
                label: named_children(node)
                    .into_iter()
                    .find(|child| child.kind() == "label")
                    .map(|child| text(child, self.source).to_owned()),
            },
            "struct_expression" => {
                let Some(name) = node.child_by_field_name("name") else {
                    return self.verbatim(node);
                };
                let Some(body) = node.child_by_field_name("body") else {
                    return self.verbatim(node);
                };
                let mut fields = Vec::new();
                for child in named_children(body) {
                    match child.kind() {
                        "field_initializer" => {
                            let (Some(field), Some(value)) = (
                                child.child_by_field_name("field"),
                                child.child_by_field_name("value"),
                            ) else {
                                return self.verbatim(node);
                            };
                            fields.push(FieldInit::Named {
                                name: text(field, self.source).to_owned(),
                                value: self.expression(value),
                            });
                        }
                        "shorthand_field_initializer" => {
                            fields.push(FieldInit::Shorthand(text(child, self.source).to_owned()))
                        }
                        "base_field_initializer" => {
                            let Some(inner) = named_children(child).into_iter().next() else {
                                return self.verbatim(node);
                            };
                            fields.push(FieldInit::Base(self.expression(inner)));
                        }
                        _ => return self.verbatim(node),
                    }
                }
                Expr::Struct {
                    path: text(name, self.source).to_owned(),
                    fields,
                }
            }
            "tuple_expression" => Expr::Tuple(
                named_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            ),
            "array_expression" => {
                let parts = named_children(node);
                // `[value; count]` has a `;` token between two expressions.
                if children(node).iter().any(|child| child.kind() == ";") && parts.len() == 2 {
                    return Expr::Array {
                        elements: vec![self.expression(parts[0])],
                        repeat: Some(Box::new(self.expression(parts[1]))),
                    };
                }
                Expr::Array {
                    elements: parts
                        .into_iter()
                        .map(|child| self.expression(child))
                        .collect(),
                    repeat: None,
                }
            }
            "index_expression" => {
                let parts = named_children(node);
                if parts.len() != 2 {
                    return self.verbatim(node);
                }
                Expr::Index {
                    value: Box::new(self.expression(parts[0])),
                    index: Box::new(self.expression(parts[1])),
                }
            }
            "range_expression" => {
                let Some(operator) = children(node)
                    .into_iter()
                    .find(|child| matches!(child.kind(), ".." | "..=" | "..."))
                else {
                    return self.verbatim(node);
                };
                let operator_start = operator.start_byte();
                let parts = named_children(node);
                let start = parts
                    .iter()
                    .find(|child| child.end_byte() <= operator_start)
                    .map(|child| Box::new(self.expression(*child)));
                let end = parts
                    .iter()
                    .find(|child| child.start_byte() >= operator.end_byte())
                    .map(|child| Box::new(self.expression(*child)));
                Expr::Range {
                    start,
                    operator: text(operator, self.source).to_owned(),
                    end,
                }
            }
            "macro_invocation" => {
                let Some(path) = node.child_by_field_name("macro") else {
                    return self.verbatim(node);
                };
                let Some(tree) = named_children(node)
                    .into_iter()
                    .find(|child| child.kind() == "token_tree")
                else {
                    return self.verbatim(node);
                };
                let raw = text(tree, self.source);
                let delimiter = match raw.chars().next() {
                    Some('[') => Delimiter::Bracket,
                    Some('{') => Delimiter::Brace,
                    _ => Delimiter::Parenthesis,
                };
                // The tokens are kept exactly, without the delimiters.
                let inner = raw
                    .get(1..raw.len().saturating_sub(1))
                    .unwrap_or_default()
                    .to_owned();
                Expr::Macro {
                    path: text(path, self.source).to_owned(),
                    delimiter,
                    tokens: inner,
                }
            }
            _ => self.verbatim(node),
        }
    }

    fn call(&self, node: Node<'a>) -> Expr {
        let Some(function) = node.child_by_field_name("function") else {
            return self.verbatim(node);
        };
        // A method call is a call whose callee is a field access. Held apart
        // because a receiver is not an argument.
        let (bare, turbofish) = if function.kind() == "generic_function" {
            let Some(inner) = function.child_by_field_name("function") else {
                return self.verbatim(node);
            };
            let arguments = function
                .child_by_field_name("type_arguments")
                .map(|child| text(child, self.source).to_owned());
            (inner, arguments)
        } else {
            (function, None)
        };
        if bare.kind() == "field_expression" {
            let (Some(receiver), Some(name)) = (
                bare.child_by_field_name("value"),
                bare.child_by_field_name("field"),
            ) else {
                return self.verbatim(node);
            };
            // `tuple.0(..)` is a call through a field, not a method.
            let name = text(name, self.source);
            if name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
            {
                return Expr::Call {
                    function: Box::new(self.expression(function)),
                    arguments: self.arguments(node),
                };
            }
            return Expr::MethodCall {
                receiver: Box::new(self.expression(receiver)),
                name: name.to_owned(),
                turbofish,
                arguments: self.arguments(node),
            };
        }
        Expr::Call {
            function: Box::new(self.expression(function)),
            arguments: self.arguments(node),
        }
    }

    fn match_expression(&self, node: Node<'a>) -> Expr {
        let (Some(value), Some(body)) = (
            node.child_by_field_name("value"),
            node.child_by_field_name("body"),
        ) else {
            return self.verbatim(node);
        };
        let mut arms = Vec::new();
        for child in named_children(body) {
            if child.kind() != "match_arm" {
                // A comment between arms. Dropping it would delete an
                // explanation, and there is nowhere to hang it, so the whole
                // match is kept as text.
                if matches!(child.kind(), "line_comment" | "block_comment") {
                    return self.verbatim(node);
                }
                continue;
            }
            let (Some(pattern), Some(arm_value)) = (
                child.child_by_field_name("pattern"),
                child.child_by_field_name("value"),
            ) else {
                return self.verbatim(node);
            };
            // `match_pattern` wraps the pattern and any guard.
            let Some(bound) = named_children(pattern).into_iter().next() else {
                return self.verbatim(node);
            };
            let guard = pattern
                .child_by_field_name("condition")
                .map(|child| self.expression(child));
            arms.push(Arm {
                attributes: self.attributes(child),
                pattern: self.pattern(bound),
                guard,
                body: self.expression(arm_value),
                comma: followed_by(child, ","),
            });
        }
        Expr::Match {
            scrutinee: Box::new(self.expression(value)),
            arms,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn pattern(&self, node: Node<'a>) -> Pat {
        match node.kind() {
            "identifier" => Pat::Binding {
                by_reference: false,
                mutable: false,
                name: text(node, self.source).to_owned(),
                subpattern: None,
            },
            "scoped_identifier" => Pat::Path(text(node, self.source).to_owned()),
            // `_` is a bare token rather than a named node in this grammar, so
            // it arrives under its own text.
            "wildcard_pattern" | "_" => Pat::Wild,
            "remaining_field_pattern" => Pat::Rest,
            "integer_literal" | "float_literal" | "string_literal" | "raw_string_literal"
            | "boolean_literal" | "char_literal" | "negative_literal" => {
                Pat::Literal(text(node, self.source).to_owned())
            }
            "mut_pattern" => {
                let Some(inner) = named_children(node)
                    .into_iter()
                    .find(|child| child.kind() != "mutable_specifier")
                else {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                };
                match self.pattern(inner) {
                    Pat::Binding {
                        by_reference,
                        name,
                        subpattern,
                        ..
                    } => Pat::Binding {
                        by_reference,
                        mutable: true,
                        name,
                        subpattern,
                    },
                    _ => Pat::Verbatim(text(node, self.source).to_owned()),
                }
            }
            "ref_pattern" => {
                let Some(inner) = named_children(node).into_iter().next() else {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                };
                match self.pattern(inner) {
                    Pat::Binding {
                        mutable,
                        name,
                        subpattern,
                        ..
                    } => Pat::Binding {
                        by_reference: true,
                        mutable,
                        name,
                        subpattern,
                    },
                    _ => Pat::Verbatim(text(node, self.source).to_owned()),
                }
            }
            "reference_pattern" => {
                let Some(inner) = named_children(node)
                    .into_iter()
                    .find(|child| child.kind() != "mutable_specifier")
                else {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                };
                Pat::Reference {
                    mutable: children(node)
                        .iter()
                        .any(|c| c.kind() == "mutable_specifier"),
                    pattern: Box::new(self.pattern(inner)),
                }
            }
            "tuple_struct_pattern" => {
                let Some(path) = node.child_by_field_name("type") else {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                };
                // `_` is anonymous in this grammar, so requiring a named child
                // silently drops the discarded positions and turns `Some(_)`
                // into `Some()`. Punctuation is excluded by name instead.
                let elements = children(node)
                    .into_iter()
                    .filter(|child| {
                        child.id() != path.id()
                            && !matches!(
                                child.kind(),
                                "(" | ")" | "," | "line_comment" | "block_comment"
                            )
                    })
                    .map(|child| self.pattern(child))
                    .collect();
                Pat::TupleStruct {
                    path: text(path, self.source).to_owned(),
                    elements,
                }
            }
            "struct_pattern" => {
                let Some(path) = node.child_by_field_name("type") else {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                };
                let mut fields = Vec::new();
                let mut rest = false;
                for child in named_children(node) {
                    if child.id() == path.id() {
                        continue;
                    }
                    match child.kind() {
                        "remaining_field_pattern" => rest = true,
                        "field_pattern" => {
                            let name = child
                                .child_by_field_name("name")
                                .map(|name| text(name, self.source).to_owned());
                            let bound = child.child_by_field_name("pattern");
                            match (name, bound) {
                                (Some(name), Some(bound)) => fields.push(FieldPat::Named {
                                    name,
                                    pattern: self.pattern(bound),
                                }),
                                // `Point { x }`, and `ref mut` forms of it.
                                (Some(name), None) => fields.push(FieldPat::Shorthand {
                                    by_reference: children(child)
                                        .iter()
                                        .any(|token| token.kind() == "ref"),
                                    mutable: children(child)
                                        .iter()
                                        .any(|token| token.kind() == "mutable_specifier"),
                                    name,
                                }),
                                _ => return Pat::Verbatim(text(node, self.source).to_owned()),
                            }
                        }
                        "shorthand_field_identifier" => fields.push(FieldPat::Shorthand {
                            by_reference: false,
                            mutable: false,
                            name: text(child, self.source).to_owned(),
                        }),
                        _ => return Pat::Verbatim(text(node, self.source).to_owned()),
                    }
                }
                Pat::Struct {
                    path: text(path, self.source).to_owned(),
                    fields,
                    rest,
                }
            }
            // `_` is anonymous in the grammar, so the discarded positions of a
            // tuple pattern are only reachable through every child rather than
            // the named ones. Collecting only the named children is what made
            // `(key, _)` and `(_, value)` the same pattern in the `Form` lift.
            "tuple_pattern" => Pat::Tuple(self.pattern_parts(node)),
            "slice_pattern" => Pat::Slice(self.pattern_parts(node)),
            "or_pattern" => Pat::Or(self.pattern_parts(node)),
            "captured_pattern" => {
                let parts = named_children(node);
                if parts.len() != 2 {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                }
                match self.pattern(parts[0]) {
                    Pat::Binding {
                        by_reference,
                        mutable,
                        name,
                        ..
                    } => Pat::Binding {
                        by_reference,
                        mutable,
                        name,
                        subpattern: Some(Box::new(self.pattern(parts[1]))),
                    },
                    _ => Pat::Verbatim(text(node, self.source).to_owned()),
                }
            }
            "range_pattern" => {
                let Some(operator) = children(node)
                    .into_iter()
                    .find(|child| matches!(child.kind(), ".." | "..=" | "..."))
                else {
                    return Pat::Verbatim(text(node, self.source).to_owned());
                };
                let parts = named_children(node);
                Pat::Range {
                    start: parts
                        .iter()
                        .find(|child| child.end_byte() <= operator.start_byte())
                        .map(|child| text(*child, self.source).to_owned()),
                    operator: text(operator, self.source).to_owned(),
                    end: parts
                        .iter()
                        .find(|child| child.start_byte() >= operator.end_byte())
                        .map(|child| text(*child, self.source).to_owned()),
                }
            }
            _ => Pat::Verbatim(text(node, self.source).to_owned()),
        }
    }

    /// A pattern's parts, including the ones the grammar leaves anonymous.
    fn pattern_parts(&self, node: Node<'a>) -> Vec<Pat> {
        children(node)
            .into_iter()
            .filter(|child| {
                !matches!(
                    child.kind(),
                    "(" | ")" | "[" | "]" | "," | "|" | "line_comment" | "block_comment"
                )
            })
            .map(|child| self.pattern(child))
            .collect()
    }
}

fn collect_functions<'a>(node: Node<'a>, output: &mut Vec<Node<'a>>) {
    if node.kind() == "function_item" {
        output.push(node);
    }
    for child in named_children(node) {
        collect_functions(child, output);
    }
}

/// Lift every function body in a parsed file.
///
/// Nested functions are skipped: an outer body already holds the inner one as
/// an item, and lifting both would replace the same bytes twice.
#[must_use]
pub fn lift_file(file: &ParsedFile) -> Vec<LiftedBody> {
    let mut nodes = Vec::new();
    collect_functions(file.tree.root_node(), &mut nodes);
    let lifter = Lifter::new(&file.source);
    let mut lifted: Vec<LiftedBody> = Vec::new();
    for node in nodes {
        let (Some(name), Some(body)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("body"),
        ) else {
            continue;
        };
        let start = body.start_byte() as u64;
        if lifted
            .iter()
            .any(|existing| start >= existing.start_byte && start < existing.end_byte)
        {
            continue;
        }
        lifted.push(LiftedBody {
            name: text(name, &file.source).to_owned(),
            start_byte: start,
            end_byte: body.end_byte() as u64,
            start_line: u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX),
            block: lifter.block(body),
        });
    }
    lifted
}
