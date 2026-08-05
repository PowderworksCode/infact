//! The faithful tree back into Rust source.
//!
//! Layout is deliberately plain: `rustfmt` is the authority on how Rust looks,
//! and a printer competing with it would be a second one. What this owes is a
//! program that means what the tree means, with every parenthesis that decides
//! an operator's reach still present.

use crate::syntax::{Arm, Block, Capture, Condition, Expr, FieldInit, FieldPat, Pat, Stmt};

fn indent(level: usize) -> String {
    "    ".repeat(level)
}

/// Print a body, braces included, at the given indentation.
#[must_use]
pub fn block(block: &Block, level: usize) -> String {
    if block.statements.is_empty() {
        return "{}".to_owned();
    }
    let mut lines: Vec<String> = Vec::new();
    for statement in &block.statements {
        if let Stmt::Comment {
            text,
            trailing: true,
        } = statement
            && let Some(last) = lines.last_mut()
        {
            last.push(' ');
            last.push_str(text);
            continue;
        }
        lines.push(self::statement(statement, level + 1));
    }
    format!("{{\n{}\n{}}}", lines.join("\n"), indent(level))
}

fn attributes(attributes: &[String], level: usize) -> String {
    attributes
        .iter()
        .map(|attribute| format!("{}{attribute}\n", indent(level)))
        .collect()
}

#[must_use]
pub fn statement(statement: &Stmt, level: usize) -> String {
    let pad = indent(level);
    match statement {
        Stmt::Comment { text, .. } => format!("{pad}{text}"),
        Stmt::Item(text) => format!("{pad}{text}"),
        Stmt::Let {
            attributes: attrs,
            pattern,
            annotation,
            value,
            diverging,
        } => {
            let mut output = attributes(attrs, level);
            output.push_str(&pad);
            output.push_str("let ");
            output.push_str(&self::pattern(pattern));
            if let Some(annotation) = annotation {
                output.push_str(": ");
                output.push_str(annotation);
            }
            if let Some(value) = value {
                output.push_str(" = ");
                output.push_str(&expression(value, level));
            }
            if let Some(diverging) = diverging {
                output.push_str(" else ");
                output.push_str(&block(diverging, level));
            }
            output.push(';');
            output
        }
        Stmt::Expr {
            attributes: attrs,
            value,
            semicolon,
        } => {
            let mut output = attributes(attrs, level);
            output.push_str(&pad);
            output.push_str(&expression(value, level));
            if *semicolon {
                output.push(';');
            }
            output
        }
    }
}

fn condition(condition: &Condition, level: usize) -> String {
    match condition {
        Condition::Plain(value) => expression(value, level),
        Condition::Let { pattern, value } => {
            format!(
                "let {} = {}",
                self::pattern(pattern),
                expression(value, level)
            )
        }
        Condition::Chain(parts) => parts
            .iter()
            .map(|part| self::condition(part, level))
            .collect::<Vec<_>>()
            .join(" && "),
    }
}

fn arm(arm: &Arm, level: usize) -> String {
    let mut output = attributes(&arm.attributes, level);
    output.push_str(&indent(level));
    output.push_str(&pattern(&arm.pattern));
    if let Some(guard) = &arm.guard {
        output.push_str(" if ");
        output.push_str(&expression(guard, level));
    }
    output.push_str(" => ");
    output.push_str(&expression(&arm.body, level));
    // A trailing comma is always valid after an arm, and is required after one
    // whose body is not a block.
    output.push(',');
    output
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn expression(value: &Expr, level: usize) -> String {
    match value {
        Expr::Verbatim(text) => text.clone(),
        Expr::Path(path) => path.clone(),
        Expr::Literal(literal) => literal.clone(),
        Expr::Unit => "()".to_owned(),
        Expr::Field { value, name } => format!("{}.{name}", expression(value, level)),
        Expr::Call {
            function,
            arguments,
        } => format!(
            "{}({})",
            expression(function, level),
            list(arguments, level)
        ),
        Expr::MethodCall {
            receiver,
            name,
            turbofish,
            arguments,
        } => format!(
            "{}.{name}{}({})",
            expression(receiver, level),
            turbofish
                .as_ref()
                .map_or_else(String::new, |arguments| format!("::{arguments}")),
            list(arguments, level)
        ),
        Expr::Binary {
            left,
            operator,
            right,
        } => format!(
            "{} {operator} {}",
            expression(left, level),
            expression(right, level)
        ),
        Expr::Unary { operator, operand } => {
            format!("{operator}{}", expression(operand, level))
        }
        Expr::Reference { mutable, value } => format!(
            "&{}{}",
            if *mutable { "mut " } else { "" },
            expression(value, level)
        ),
        Expr::Assign {
            target,
            operator,
            value,
        } => format!(
            "{} {operator} {}",
            expression(target, level),
            expression(value, level)
        ),
        Expr::Cast { value, annotation } => {
            format!("{} as {annotation}", expression(value, level))
        }
        Expr::Try(value) => format!("{}?", expression(value, level)),
        Expr::Await(value) => format!("{}.await", expression(value, level)),
        Expr::Parenthesized(value) => format!("({})", expression(value, level)),
        Expr::Closure {
            capture,
            asynchronous,
            parameters,
            annotation,
            body,
        } => {
            let mut output = String::new();
            if *asynchronous {
                output.push_str("async ");
            }
            if matches!(capture, Capture::ByValue) {
                output.push_str("move ");
            }
            output.push('|');
            output.push_str(
                &parameters
                    .iter()
                    .map(pattern)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            output.push('|');
            // The grammar's `return_type` field is the type alone, so the
            // arrow has to be put back or the closure reads as a comparison.
            if let Some(annotation) = annotation {
                output.push_str(" -> ");
                output.push_str(annotation);
            }
            output.push(' ');
            output.push_str(&expression(body, level));
            output
        }
        Expr::If {
            condition: test,
            consequence,
            alternative,
        } => {
            let mut output = format!(
                "if {} {}",
                condition(test, level),
                block(consequence, level)
            );
            if let Some(alternative) = alternative {
                output.push_str(" else ");
                output.push_str(&expression(alternative, level));
            }
            output
        }
        Expr::Match { scrutinee, arms } => {
            if arms.is_empty() {
                return format!("match {} {{}}", expression(scrutinee, level));
            }
            let body = arms
                .iter()
                .map(|entry| arm(entry, level + 1))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "match {} {{\n{body}\n{}}}",
                expression(scrutinee, level),
                indent(level)
            )
        }
        Expr::While {
            label,
            condition: test,
            body,
        } => format!(
            "{}while {} {}",
            label_prefix(label.as_deref()),
            condition(test, level),
            block(body, level)
        ),
        Expr::For {
            label,
            pattern: bound,
            sequence,
            body,
        } => format!(
            "{}for {} in {} {}",
            label_prefix(label.as_deref()),
            pattern(bound),
            expression(sequence, level),
            block(body, level)
        ),
        Expr::Loop { label, body } => format!(
            "{}loop {}",
            label_prefix(label.as_deref()),
            block(body, level)
        ),
        Expr::Block {
            label,
            modifiers,
            body,
        } => {
            let mut output = label_prefix(label.as_deref());
            for modifier in modifiers {
                output.push_str(modifier);
                output.push(' ');
            }
            output.push_str(&block(body, level));
            output
        }
        Expr::Return(value) => match value {
            Some(value) => format!("return {}", expression(value, level)),
            None => "return".to_owned(),
        },
        Expr::Break { label, value } => {
            let mut output = String::from("break");
            if let Some(label) = label {
                output.push(' ');
                output.push_str(label);
            }
            if let Some(value) = value {
                output.push(' ');
                output.push_str(&expression(value, level));
            }
            output
        }
        Expr::Continue { label } => match label {
            Some(label) => format!("continue {label}"),
            None => "continue".to_owned(),
        },
        Expr::Struct { path, fields } => {
            if fields.is_empty() {
                return format!("{path} {{}}");
            }
            let body = fields
                .iter()
                .map(|field| match field {
                    FieldInit::Named { name, value } => {
                        format!(
                            "{}{name}: {}",
                            indent(level + 1),
                            expression(value, level + 1)
                        )
                    }
                    FieldInit::Shorthand(name) => format!("{}{name}", indent(level + 1)),
                    FieldInit::Base(value) => {
                        format!("{}..{}", indent(level + 1), expression(value, level + 1))
                    }
                })
                .collect::<Vec<_>>()
                .join(",\n");
            format!("{path} {{\n{body}\n{}}}", indent(level))
        }
        // A one-element tuple needs its comma, or it is a parenthesized
        // expression instead.
        Expr::Tuple(parts) => match parts.as_slice() {
            [only] => format!("({},)", expression(only, level)),
            _ => format!("({})", list(parts, level)),
        },
        Expr::Array { elements, repeat } => match repeat {
            Some(count) => format!(
                "[{}; {}]",
                elements
                    .first()
                    .map_or_else(String::new, |value| expression(value, level)),
                expression(count, level)
            ),
            None => format!("[{}]", list(elements, level)),
        },
        Expr::Index { value, index } => {
            format!("{}[{}]", expression(value, level), expression(index, level))
        }
        Expr::Range {
            start,
            operator,
            end,
        } => format!(
            "{}{operator}{}",
            start
                .as_ref()
                .map_or_else(String::new, |value| expression(value, level)),
            end.as_ref()
                .map_or_else(String::new, |value| expression(value, level))
        ),
        Expr::Macro {
            path,
            delimiter,
            tokens,
        } => format!("{path}!{}{tokens}{}", delimiter.open(), delimiter.close()),
    }
}

fn label_prefix(label: Option<&str>) -> String {
    label.map_or_else(String::new, |label| format!("{label}: "))
}

fn list(values: &[Expr], level: usize) -> String {
    values
        .iter()
        .map(|value| expression(value, level))
        .collect::<Vec<_>>()
        .join(", ")
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn pattern(value: &Pat) -> String {
    match value {
        Pat::Verbatim(text) => text.clone(),
        Pat::Wild => "_".to_owned(),
        Pat::Rest => "..".to_owned(),
        Pat::Path(path) => path.clone(),
        Pat::Literal(literal) => literal.clone(),
        Pat::Binding {
            by_reference,
            mutable,
            name,
            subpattern,
        } => {
            let mut output = String::new();
            if *by_reference {
                output.push_str("ref ");
            }
            if *mutable {
                output.push_str("mut ");
            }
            output.push_str(name);
            if let Some(subpattern) = subpattern {
                output.push_str(" @ ");
                output.push_str(&pattern(subpattern));
            }
            output
        }
        Pat::TupleStruct { path, elements } => format!(
            "{path}({})",
            elements.iter().map(pattern).collect::<Vec<_>>().join(", ")
        ),
        Pat::Struct { path, fields, rest } => {
            let mut parts = fields
                .iter()
                .map(|field| match field {
                    FieldPat::Named {
                        name,
                        pattern: bound,
                    } => {
                        format!("{name}: {}", pattern(bound))
                    }
                    FieldPat::Shorthand {
                        by_reference,
                        mutable,
                        name,
                    } => {
                        let mut output = String::new();
                        if *by_reference {
                            output.push_str("ref ");
                        }
                        if *mutable {
                            output.push_str("mut ");
                        }
                        output.push_str(name);
                        output
                    }
                })
                .collect::<Vec<_>>();
            if *rest {
                parts.push("..".to_owned());
            }
            if parts.is_empty() {
                return format!("{path} {{}}");
            }
            format!("{path} {{ {} }}", parts.join(", "))
        }
        Pat::Tuple(parts) => format!(
            "({})",
            parts.iter().map(pattern).collect::<Vec<_>>().join(", ")
        ),
        Pat::Slice(parts) => format!(
            "[{}]",
            parts.iter().map(pattern).collect::<Vec<_>>().join(", ")
        ),
        Pat::Or(parts) => parts.iter().map(pattern).collect::<Vec<_>>().join(" | "),
        Pat::Reference {
            mutable,
            pattern: bound,
        } => format!("&{}{}", if *mutable { "mut " } else { "" }, pattern(bound)),
        Pat::Range {
            start,
            operator,
            end,
        } => format!(
            "{}{operator}{}",
            start.clone().unwrap_or_default(),
            end.clone().unwrap_or_default()
        ),
    }
}
