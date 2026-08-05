//! Normalizes C syntax into Infact's behavior form.
//!
//! This module knows C and nothing else: no library, no project, no macro
//! dialect. It reads the tree the C parser pack produced — after entl's
//! dialect rewrites, so git-flavored attribute and iterator macros have
//! already been neutralized — and reduces what remains to the same
//! [`Form`] every other language reduces to.
//!
//! The canonicalizations, in order of how much they matter:
//!
//! 1. **Iteration form.** The canonical C loop is an index walk —
//!    `for (i = 0; i < n; i++) { .. a[i] .. }` — and it describes the same
//!    traversal as Rust's `for x in a` and JavaScript's `for (const x of a)`.
//!    C has no other spelling of its own: every list macro the dialect table
//!    rewrote away expands to exactly this. Without this rule nothing else in
//!    the module is worth anything.
//! 2. **Pointer ceremony.** `*p`, `&x`, and casts are how C passes things
//!    around, not what the code does. All three reduce to what they wrap. The
//!    cost is real — `*p = 1` and `p = 1` become the same assignment — and
//!    accepted: ownership is the port planner's problem, behavior matching is
//!    this module's, and a hole in the middle of every dereferencing
//!    expression would match nothing at all.
//! 3. **Blocks and statement expressions.** A block wrapping one statement is
//!    that statement; a comma expression is a sequence.
//!
//! What stays [`Form::Opaque`], deliberately: `goto` and labels (no canonical
//! shape — a cleanup `goto` is a return path, a retry `goto` is a loop, and
//! guessing which would corrupt matches), `do`/`while` (rare and almost never
//! an element walk), pointer-chasing loops (`for (p = head; p; p = p->next)`
//! — a traversal, but of a sequence spelled by a *type's* convention rather
//! than the language's; recognizing it needs the linked-list shape named,
//! which is a corpus fact, not a language fact).

use entl_tree_sitter::ParsedFile;
use infact_normalize::{Arm, Direction, Form, Pattern, Roles};
use tree_sitter::Node;

/// A named function found in a parsed file, with the body normalized.
#[derive(Debug, Clone)]
pub struct NormalizedFunction {
    pub name: String,
    pub form: Form,
    /// 1-based line where the function starts, for reporting.
    pub line: usize,
}

/// Normalize every function definition in a parsed C file.
pub fn normalize_file(file: &ParsedFile) -> Vec<NormalizedFunction> {
    let source = file.source.as_ref();
    let mut out = Vec::new();
    collect_functions(file.tree.root_node(), source, &mut out);
    out
}

fn collect_functions(node: Node<'_>, source: &[u8], out: &mut Vec<NormalizedFunction>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                if let Some(name) = function_name(child, source) {
                    let form = normalize_function(child, source);
                    out.push(NormalizedFunction {
                        name,
                        form,
                        line: child.start_position().row + 1,
                    });
                }
            }
            // Functions hide inside #ifdef blocks and linkage blocks.
            "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "linkage_specification"
            | "declaration_list" => {
                collect_functions(child, source, out);
            }
            _ => {}
        }
    }
}

/// Normalize one `function_definition`'s body.
pub fn normalize_function(function: Node<'_>, source: &[u8]) -> Form {
    let mut normalizer = Normalizer::new(source);
    // Parameters are the free variables of the behavior, in order.
    if let Some(declarator) = function.child_by_field_name("declarator") {
        for parameter in parameter_names(declarator, source) {
            normalizer.roles.declare(&parameter);
        }
    }
    function
        .child_by_field_name("body")
        .and_then(|body| normalizer.statement(body))
        .unwrap_or(Form::Literal)
}

/// Normalize a bare statement or block, with no surrounding function.
pub fn normalize_body(body: Node<'_>, source: &[u8]) -> Form {
    let mut normalizer = Normalizer::new(source);
    normalizer.statement(body).unwrap_or(Form::Literal)
}

struct Normalizer<'a> {
    source: &'a [u8],
    roles: Roles,
    /// While inside a recognized index walk: (sequence text, counter text,
    /// item local), so `a[i]` normalizes straight to the item.
    walks: Vec<(String, String, u32)>,
}

impl<'a> Normalizer<'a> {
    fn new(source: &'a [u8]) -> Self {
        Normalizer {
            source,
            roles: Roles::new(),
            walks: Vec::new(),
        }
    }

    fn text(&self, node: Node<'_>) -> &'a str {
        std::str::from_utf8(&self.source[node.byte_range()]).unwrap_or("")
    }

    fn statement(&mut self, node: Node<'_>) -> Option<Form> {
        match node.kind() {
            "compound_statement" => {
                let mut steps = Vec::new();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "comment" {
                        continue;
                    }
                    if let Some(step) = self.statement(child) {
                        steps.push(step);
                    }
                }
                Some(match steps.len() {
                    0 => Form::Literal,
                    1 => steps.into_iter().next()?,
                    _ => Form::Sequence(steps),
                })
            }
            "declaration" => self.declaration(node),
            "expression_statement" => {
                let expression = node.named_child(0)?;
                Some(self.expression(expression))
            }
            "if_statement" => self.if_statement(node),
            "switch_statement" => self.switch_statement(node),
            "for_statement" => self.for_statement(node),
            "while_statement" => self.while_statement(node),
            "return_statement" => {
                let value = node
                    .named_child(0)
                    .map_or(Form::Literal, |value| self.expression(value));
                Some(Form::Return(Box::new(value)))
            }
            "break_statement" | "continue_statement" => Some(Form::Opaque {
                kind: node.kind().trim_end_matches("_statement").to_owned(),
                parts: Vec::new(),
            }),
            "labeled_statement" => {
                // The label is control-flow bookkeeping; the statement is code.
                let last = u32::try_from(node.named_child_count().saturating_sub(1)).ok()?;
                let inner = node.named_child(last)?;
                self.statement(inner)
            }
            "goto_statement" => Some(Form::Opaque {
                kind: "goto".to_owned(),
                parts: Vec::new(),
            }),
            "do_statement" => Some(Form::Opaque {
                kind: "do".to_owned(),
                parts: node
                    .child_by_field_name("body")
                    .and_then(|body| self.statement(body))
                    .into_iter()
                    .collect(),
            }),
            "comment" | "preproc_call" | ";" => None,
            _ => Some(Form::Opaque {
                kind: node.kind().to_owned(),
                parts: Vec::new(),
            }),
        }
    }

    /// `T x = e;` is a let; `T x;` binds a name and says nothing yet.
    fn declaration(&mut self, node: Node<'_>) -> Option<Form> {
        let mut lets = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() != "init_declarator" {
                // A bare declarator still binds its name, so later mentions
                // resolve to a local rather than a free variable.
                if let Some(name) = innermost_identifier(child, self.source) {
                    self.roles.declare(&name);
                }
                continue;
            }
            let declarator = child.child_by_field_name("declarator")?;
            let name = innermost_identifier(declarator, self.source)?;
            let value = child.child_by_field_name("value")?;
            let value = self.expression(value);
            let Form::Local(index) = self.roles.bind(&name) else {
                continue;
            };
            lets.push(Form::Let {
                pattern: Box::new(Pattern::Binding(index)),
                value: Box::new(value),
            });
        }
        match lets.len() {
            0 => None,
            1 => lets.into_iter().next(),
            _ => Some(Form::Sequence(lets)),
        }
    }

    fn if_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let condition = node.child_by_field_name("condition")?;
        let condition = self.expression(condition);
        let consequence = node
            .child_by_field_name("consequence")
            .and_then(|consequence| self.statement(consequence))
            .unwrap_or(Form::Literal);
        let alternative = node
            .child_by_field_name("alternative")
            .and_then(|alternative| alternative.named_child(0))
            .and_then(|alternative| self.statement(alternative));
        Some(Form::Branch {
            condition: Box::new(condition),
            consequence: Box::new(consequence),
            alternative: alternative.map(Box::new),
        })
    }

    /// A `switch` is a select; each `case` is an arm named by its value.
    fn switch_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let scrutinee = node.child_by_field_name("condition")?;
        let scrutinee = self.expression(scrutinee);
        let body = node.child_by_field_name("body")?;
        let mut arms = Vec::new();
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() != "case_statement" {
                continue;
            }
            let name = child
                .child_by_field_name("value")
                .map_or("default".to_owned(), |value| self.text(value).to_owned());
            let mut steps = Vec::new();
            let mut arm_cursor = child.walk();
            for grandchild in child.named_children(&mut arm_cursor) {
                if grandchild.kind() == "comment"
                    || Some(grandchild.id())
                        == child.child_by_field_name("value").map(|value| value.id())
                {
                    continue;
                }
                if let Some(step) = self.statement(grandchild) {
                    steps.push(step);
                }
            }
            let body = match steps.len() {
                0 => Form::Literal,
                1 => steps.into_iter().next()?,
                _ => Form::Sequence(steps),
            };
            arms.push(Arm {
                pattern: Pattern::Variant {
                    name,
                    parts: Vec::new(),
                },
                body,
            });
        }
        Some(Form::select(scrutinee, arms))
    }

    /// An index walk is a traversal; anything else keeps its shape.
    fn for_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let body = node.child_by_field_name("body")?;
        let Some((counter, direction)) = self.loop_counter(node) else {
            return Some(Form::Opaque {
                kind: "for".to_owned(),
                parts: self.statement(body).into_iter().collect(),
            });
        };
        let Some(indexed) = find_indexed(body, &counter, self.source) else {
            return Some(Form::Opaque {
                kind: "for".to_owned(),
                parts: self.statement(body).into_iter().collect(),
            });
        };
        let sequence_text = self.text(indexed.sequence).to_owned();
        let sequence = self.expression(indexed.sequence);
        let binding = indexed
            .binding
            .clone()
            .unwrap_or_else(|| format!("<{counter}>"));
        let Form::Local(index) = self.roles.bind(&binding) else {
            return None;
        };
        self.walks.push((sequence_text, counter.clone(), index));
        let mut steps = Vec::new();
        let mut cursor = body.walk();
        let children: Vec<Node<'_>> = if body.kind() == "compound_statement" {
            body.named_children(&mut cursor).collect()
        } else {
            vec![body]
        };
        for child in children {
            if child.kind() == "comment" {
                continue;
            }
            // The declaration that named the element was consumed by the bind.
            if indexed
                .declaration
                .is_some_and(|declaration| declaration.id() == child.id())
            {
                continue;
            }
            if let Some(step) = self.statement(child) {
                steps.push(step);
            }
        }
        self.walks.pop();
        let walked = match steps.len() {
            0 => Form::Literal,
            1 => steps.into_iter().next()?,
            _ => Form::Sequence(steps),
        };
        Some(Form::Traverse {
            sequence: Box::new(sequence),
            item: Box::new(Pattern::Binding(index)),
            body: Box::new(walked),
            direction,
        })
    }

    /// The name a `for` header counts with, and which way it runs.
    fn loop_counter(&self, node: Node<'_>) -> Option<(String, Direction)> {
        let initializer = node.child_by_field_name("initializer")?;
        // C spells it two ways: `int i = 0` (declaration) or `i = 0`
        // (assignment). Both give a name and a starting value.
        let (name, value) = match initializer.kind() {
            "declaration" => {
                let declarator = descendant(initializer, "init_declarator")?;
                let name = declarator.child_by_field_name("declarator")?;
                let value = declarator.child_by_field_name("value")?;
                (name, value)
            }
            "assignment_expression" => {
                let name = initializer.child_by_field_name("left")?;
                let value = initializer.child_by_field_name("right")?;
                (name, value)
            }
            _ => return None,
        };
        let counter = self.text(name).to_owned();
        let starts_at_zero = self.text(value).trim() == "0";
        let condition = node.child_by_field_name("condition")?;
        let bounded = condition
            .child_by_field_name("left")
            .is_some_and(|left| self.text(left) == counter);
        let comparison = condition
            .child_by_field_name("operator")
            .map(|operator| self.text(operator))
            .unwrap_or_default();
        let update = node.child_by_field_name("update")?;
        let update = self.text(update);
        if !bounded || !update.contains(&counter) {
            return None;
        }
        let direction = match comparison {
            "<" | "<=" if starts_at_zero && !update.contains("--") => Direction::Forward,
            ">" | ">=" if update.contains("--") => Direction::Backward,
            _ => return None,
        };
        Some((counter, direction))
    }

    /// A counted `while` walks too: `while (i < n) { .. a[i] .. i++; }`.
    fn while_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let body = node.child_by_field_name("body")?;
        Some(Form::Opaque {
            kind: "while".to_owned(),
            parts: self.statement(body).into_iter().collect(),
        })
    }

    fn expression(&mut self, node: Node<'_>) -> Form {
        match node.kind() {
            "identifier" => {
                let name = self.text(node).to_owned();
                if name == "NULL" {
                    return Form::Constant("NULL".to_owned());
                }
                self.roles.resolve(&name)
            }
            "number_literal" => Form::Number(self.text(node).to_owned()),
            "string_literal" | "char_literal" | "concatenated_string" => {
                Form::Constant(self.text(node).to_owned())
            }
            "true" | "false" => Form::Constant(self.text(node).to_owned()),
            "null" => Form::Constant("NULL".to_owned()),
            "call_expression" => self.call(node),
            "field_expression" => {
                let value = node
                    .child_by_field_name("argument")
                    .map_or(Form::Literal, |argument| self.expression(argument));
                let name = node
                    .child_by_field_name("field")
                    .map_or_else(String::new, |field| self.text(field).to_owned());
                Form::Field {
                    value: Box::new(value),
                    name,
                }
            }
            "subscript_expression" => self.subscript(node),
            "assignment_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map_or("=", |operator| self.text(operator))
                    .to_owned();
                let target = node
                    .child_by_field_name("left")
                    .map_or(Form::Literal, |left| self.expression(left));
                let value = node
                    .child_by_field_name("right")
                    .map_or(Form::Literal, |right| self.expression(right));
                Form::Assign {
                    operator,
                    target: Box::new(target),
                    value: Box::new(value),
                }
            }
            "binary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map_or_else(String::new, |operator| self.text(operator).to_owned());
                let left = node
                    .child_by_field_name("left")
                    .map_or(Form::Literal, |left| self.expression(left));
                let right = node
                    .child_by_field_name("right")
                    .map_or(Form::Literal, |right| self.expression(right));
                Form::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
            "update_expression" => {
                // `i++` is `i += 1`: an accumulation, not a mystery.
                let argument = node
                    .child_by_field_name("argument")
                    .map_or(Form::Literal, |argument| self.expression(argument));
                let operator = if self.text(node).contains("--") {
                    "-="
                } else {
                    "+="
                };
                Form::Assign {
                    operator: operator.to_owned(),
                    target: Box::new(argument),
                    value: Box::new(Form::Number("1".to_owned())),
                }
            }
            "conditional_expression" => {
                let condition = node
                    .child_by_field_name("condition")
                    .map_or(Form::Literal, |condition| self.expression(condition));
                let consequence = node
                    .child_by_field_name("consequence")
                    .map_or(Form::Literal, |consequence| self.expression(consequence));
                let alternative = node
                    .child_by_field_name("alternative")
                    .map(|alternative| Box::new(self.expression(alternative)));
                Form::Branch {
                    condition: Box::new(condition),
                    consequence: Box::new(consequence),
                    alternative,
                }
            }
            // Pointer ceremony and grouping: reduce to what they wrap.
            "pointer_expression" | "parenthesized_expression" => node
                .named_child(0)
                .map_or(Form::Literal, |inner| self.expression(inner)),
            "cast_expression" => node
                .child_by_field_name("value")
                .map_or(Form::Literal, |value| self.expression(value)),
            "unary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map_or_else(String::new, |operator| self.text(operator).to_owned());
                let argument = node
                    .child_by_field_name("argument")
                    .map_or(Form::Literal, |argument| self.expression(argument));
                Form::Opaque {
                    kind: operator,
                    parts: vec![argument],
                }
            }
            "comma_expression" => {
                let mut parts = Vec::new();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    parts.push(self.expression(child));
                }
                Form::Sequence(parts)
            }
            "sizeof_expression" => Form::Opaque {
                kind: "sizeof".to_owned(),
                parts: Vec::new(),
            },
            _ => Form::Opaque {
                kind: node.kind().to_owned(),
                parts: Vec::new(),
            },
        }
    }

    fn call(&mut self, node: Node<'_>) -> Form {
        let callee = node
            .child_by_field_name("function")
            .map_or(Form::Literal, |function| match function.kind() {
                "identifier" => Form::Path(self.text(function).to_owned()),
                _ => self.expression(function),
            });
        let mut arguments = Vec::new();
        if let Some(list) = node.child_by_field_name("arguments") {
            let mut cursor = list.walk();
            for child in list.named_children(&mut cursor) {
                if child.kind() == "comment" {
                    continue;
                }
                arguments.push(self.expression(child));
            }
        }
        Form::Call {
            callee: Box::new(callee),
            arguments,
        }
    }

    /// `a[i]` inside a walk over `a` with counter `i` *is* the item.
    fn subscript(&mut self, node: Node<'_>) -> Form {
        let argument = node.child_by_field_name("argument");
        let index = node.child_by_field_name("index");
        if let (Some(argument), Some(index)) = (argument, index) {
            let argument_text = self.text(argument);
            let index_text = self.text(index);
            for (sequence, counter, item) in self.walks.iter().rev() {
                if sequence == argument_text && counter == index_text {
                    return Form::Local(*item);
                }
            }
        }
        let value = argument.map_or(Form::Literal, |argument| self.expression(argument));
        let at = index.map_or(Form::Literal, |index| self.expression(index));
        Form::Method {
            name: "at".to_owned(),
            receiver: Box::new(value),
            arguments: vec![at],
        }
    }
}

/// What a loop body indexes with the counter.
struct Indexed<'t> {
    sequence: Node<'t>,
    /// The name a leading `T v = a[i];` gave the element, if one did.
    binding: Option<String>,
    /// That declaration, so the caller can skip it.
    declaration: Option<Node<'t>>,
}

/// The first `a[counter]` in the body, and the declaration binding it if the
/// body opens with `T v = a[counter];`.
fn find_indexed<'t>(body: Node<'t>, counter: &str, source: &[u8]) -> Option<Indexed<'t>> {
    let text = |node: Node<'_>| std::str::from_utf8(&source[node.byte_range()]).unwrap_or("");
    // A leading declaration that names the element is the strongest signal.
    if body.kind() == "compound_statement" {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() != "declaration" {
                break;
            }
            if let Some(declarator) = descendant(child, "init_declarator")
                && let Some(value) = declarator.child_by_field_name("value")
                && value.kind() == "subscript_expression"
                && value
                    .child_by_field_name("index")
                    .is_some_and(|index| text(index) == counter)
                && let Some(sequence) = value.child_by_field_name("argument")
                && let Some(name) = declarator
                    .child_by_field_name("declarator")
                    .and_then(|declarator| innermost_identifier(declarator, source))
            {
                return Some(Indexed {
                    sequence,
                    binding: Some(name),
                    declaration: Some(child),
                });
            }
        }
    }
    // Otherwise: any subscript by the counter, anywhere in the body.
    let mut stack = vec![body];
    while let Some(current) = stack.pop() {
        if current.kind() == "subscript_expression"
            && current
                .child_by_field_name("index")
                .is_some_and(|index| text(index) == counter)
            && let Some(sequence) = current.child_by_field_name("argument")
        {
            return Some(Indexed {
                sequence,
                binding: None,
                declaration: None,
            });
        }
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// The first descendant of `kind`, depth-first.
fn descendant<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == kind {
            return Some(current);
        }
        let mut cursor = current.walk();
        let children: Vec<Node<'t>> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

/// The name a function definition declares.
fn function_name(function: Node<'_>, source: &[u8]) -> Option<String> {
    let declarator = function.child_by_field_name("declarator")?;
    innermost_identifier(declarator, source)
}

/// Parameter names, in order, from a function declarator.
fn parameter_names(declarator: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(list) = descendant(declarator, "parameter_list") {
        let mut cursor = list.walk();
        for parameter in list.named_children(&mut cursor) {
            if parameter.kind() != "parameter_declaration" {
                continue;
            }
            if let Some(inner) = parameter.child_by_field_name("declarator")
                && let Some(name) = innermost_identifier(inner, source)
            {
                names.push(name);
            }
        }
    }
    names
}

/// Descend through pointer/array/function/paren declarators to the name.
fn innermost_identifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(
            std::str::from_utf8(&source[node.byte_range()])
                .unwrap_or("")
                .to_owned(),
        ),
        "pointer_declarator"
        | "array_declarator"
        | "function_declarator"
        | "parenthesized_declarator"
        | "init_declarator" => {
            if let Some(inner) = node.child_by_field_name("declarator") {
                return innermost_identifier(inner, source);
            }
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
            for child in children {
                if let Some(name) = innermost_identifier(child, source) {
                    return Some(name);
                }
            }
            None
        }
        _ => None,
    }
}
