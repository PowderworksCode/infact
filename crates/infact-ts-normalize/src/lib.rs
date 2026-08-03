//! Normalizes TypeScript and JavaScript syntax into Infact's behavior form.
//!
//! This module knows ECMAScript and nothing else. It names no package, no
//! callable, and no API: every rule here is about how the language spells
//! something, so one implementation covers every library a repository depends
//! on.
//!
//! One normalizer serves both grammars. `tree-sitter-typescript` is an
//! extension of `tree-sitter-javascript` and spells every construct here
//! identically; the TypeScript-only node kinds are all type syntax, which is
//! annotation rather than behavior and is skipped. That matters because the
//! library implementations being derived from are JavaScript while the code
//! being analyzed is TypeScript, and a match has to bridge the two.
//!
//! The canonicalizations, in order of how much they matter:
//!
//! 1. **Iteration form.** The canonical JavaScript loop is an index walk —
//!    `for (var k = 0; k < len; k++) { ... a[k] ... }` — and it describes the
//!    same traversal as `for (const x of a)` and as `a.forEach(x => ..)`. Rust
//!    never needed this rule because `for p in e` is already the idiom there.
//!    Without it nothing else in this module is worth anything.
//! 2. **Spec ceremony.** An engine implementation opens by coercing its
//!    receiver: `ToObject(this)`, `ToLength(O.length)`. These are the abstract
//!    operations the specification is written in, they return what they were
//!    given, and they carry no behavior.
//! 3. **Call convention.** `callContentFunction(f, thisArg, x, i, o)` is how a
//!    self-hosted builtin invokes a caller's function. What it describes is
//!    `f(x)`.
//! 4. **Preconditions.** A branch whose only consequence is a throw states what
//!    the caller must not do. Every implementation has them and no caller
//!    reimplementing the behavior writes them, so they are not behavior.
//! 5. **Syntactic noise.** Type annotations, assertions, parentheses,
//!    non-null assertions, and blocks wrapping a single expression.

mod cleanup;

use cleanup::{
    drop_unused_bindings, inline_aliases, is_presence_test, push_flattened, replace_element_access,
    trim_protocol_arguments, valued,
};
use entl_tree_sitter::ParsedFile;
use infact_normalize::{Arm, Direction, Form, Pattern, Roles};
use tree_sitter::Node;

/// Abstract operations that hand back what they were given.
///
/// These are the specification's coercions. An engine writes them because it
/// must accept any receiver; a caller who already has an array does not write
/// them at all. Treating them as identity is what lets the two forms meet.
const IDENTITY_OPERATIONS: &[&str] = &[
    "ToObject",
    "ToLength",
    "ToInteger",
    "ToIntegerOrInfinity",
    "ToNumber",
    "ToUint32",
    "ToPropertyKey",
    "RequireObjectCoercible",
    "IndexedObject",
];

/// How a self-hosted builtin calls a function it was handed.
///
/// The first argument is the function and the second is the receiver to call it
/// on; the rest are the real arguments. `Function.prototype.call` has the same
/// shape and the same meaning.
const THIS_ARG_CALLS: &[&str] = &["callContentFunction", "callFunction", "call"];

/// Operations that report on a precondition rather than doing work.
const PRECONDITION_OPERATIONS: &[&str] = &[
    "IsCallable",
    "IsConstructor",
    "IsNullOrUndefined",
    "ArgumentsLength",
    "GetArgument",
    "DecompileArg",
    "IsPackedArray",
    "IsObject",
];

/// Calls whose only effect is to abandon the computation.
const THROWING_OPERATIONS: &[&str] = &["ThrowTypeError", "ThrowRangeError", "ThrowInternalError"];

/// Methods that hand back the sequence they were given.
const SEQUENCE_ADAPTERS: &[&str] = &["slice", "values", "entries", "flat", "at"];

/// A named function found in a parsed file, with the body to normalize.
#[derive(Debug, Clone)]
pub struct NormalizedFunction {
    pub name: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
    pub form: Form,
    /// Where each step of `form` came from, when `form` is a sequence.
    ///
    /// Aligned with the steps by construction rather than by recomputing the
    /// body's statements: normalization drops some statements outright — a
    /// guard that only throws, a binding nothing reads, a name bound to another
    /// name — so counting source statements separately would slide the spans
    /// against the steps and report a match at the wrong line. Silently wrong
    /// coordinates are worse than coarse ones, so they are carried along
    /// instead of reconstructed.
    pub statements: Vec<StatementSpan>,
    /// Every statement inside this function, at every depth, with its own form
    /// and span. This is what lets a match be reported at the line that carries
    /// it rather than at the function that contains it.
    pub located: Vec<LocatedForm>,
    /// Whether the grammar failed anywhere inside this function.
    ///
    /// SpiderMonkey runs its self-hosted JavaScript through the C preprocessor,
    /// so a few files carry `#if`/`#else`/`#endif` lines no JavaScript grammar
    /// reads. The damage is local: a consumer should decline the callable
    /// rather than derive a behavior from a body it only partly understood.
    pub damaged: bool,
}

/// One statement, its normalized form, and where it was written.
///
/// A behavior is usually found *inside* a function rather than being the whole
/// of one, and saying which function is not much help when the function is four
/// hundred lines. Recording every statement at every depth as it is normalized
/// gives a consumer somewhere exact to point, and gives it small forms to
/// compare instead of one enormous one.
#[derive(Debug, Clone)]
pub struct LocatedForm {
    pub span: StatementSpan,
    /// How deeply nested the statement is; 0 is the function body's own level.
    pub depth: u32,
    pub form: Form,
}

/// The source extent of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementSpan {
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub end_line: u32,
}

impl StatementSpan {
    fn of(node: Node<'_>) -> Self {
        Self {
            start_byte: node.start_byte() as u64,
            end_byte: node.end_byte() as u64,
            start_line: u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX),
            end_line: u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX),
        }
    }
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default()).unwrap_or_default()
}

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

struct Normalizer<'a> {
    source: &'a [u8],
    roles: Roles,
    /// Every statement normalized so far, with where it came from.
    located: Vec<LocatedForm>,
    depth: u32,
}

impl<'a> Normalizer<'a> {
    fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            roles: Roles::new(),
            located: Vec::new(),
            depth: 0,
        }
    }

    fn text(&self, node: Node<'_>) -> &'a str {
        text(node, self.source)
    }

    // -- statements ---------------------------------------------------------

    fn block(&mut self, node: Node<'_>) -> Form {
        self.block_located(node).0
    }

    /// A block, and where each surviving step was written.
    fn block_located(&mut self, node: Node<'_>) -> (Form, Vec<StatementSpan>) {
        let mut steps = Vec::new();
        self.depth += 1;
        for child in named_children(node) {
            if child.kind() == "comment" {
                continue;
            }
            if let Some(step) = self.statement(child) {
                push_flattened(&mut steps, step, StatementSpan::of(child), child);
            }
        }
        let steps = drop_unused_bindings(inline_aliases(steps));
        self.depth -= 1;
        let (forms, spans): (Vec<_>, Vec<_>) = steps.into_iter().unzip();
        let form = match forms.len() {
            0 => Form::Sequence(Vec::new()),
            1 => forms.into_iter().next().unwrap_or(Form::Literal),
            _ => Form::Sequence(forms),
        };
        (form, spans)
    }

    /// One statement, or nothing when it carries no behavior.
    ///
    /// Every statement that produces a form is recorded with its own span, so a
    /// match found deep inside a function can be reported where it is written.
    fn statement(&mut self, node: Node<'_>) -> Option<Form> {
        let form = self.statement_form(node)?;
        // a bare block adds nesting but no statement of its own
        if node.kind() != "statement_block" && !form.is_trivial() {
            self.located.push(LocatedForm {
                span: StatementSpan::of(node),
                depth: self.depth,
                form: form.clone(),
            });
        }
        Some(form)
    }

    fn statement_form(&mut self, node: Node<'_>) -> Option<Form> {
        match node.kind() {
            "comment" | "empty_statement" => None,
            "expression_statement" => {
                let inner = node.named_child(0)?;
                // a bare precondition call states a requirement, not behavior
                if self.is_throwing_call(inner) {
                    return None;
                }
                Some(self.expression(inner))
            }
            "return_statement" => Some(Form::Return(Box::new(match node.named_child(0) {
                Some(value) => self.expression(value),
                None => Form::Literal,
            }))),
            "statement_block" => Some(self.block(node)),
            "if_statement" => self.if_statement(node),
            "for_statement" => self.for_statement(node),
            "for_in_statement" => self.for_of_statement(node),
            "while_statement" => self.while_statement(node),
            "variable_declaration" | "lexical_declaration" => self.declaration(node),
            "switch_statement" => Some(self.switch_statement(node)),
            "throw_statement" => None,
            "break_statement" => Some(Form::Variant {
                name: "Break".to_owned(),
                payload: Vec::new(),
            }),
            "continue_statement" => Some(Form::Variant {
                name: "Continue".to_owned(),
                payload: Vec::new(),
            }),
            _ => Some(self.expression(node)),
        }
    }

    /// Whether a node is a call that only abandons the computation.
    fn is_throwing_call(&self, node: Node<'_>) -> bool {
        node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .map(|callee| self.text(callee))
                .is_some_and(|name| THROWING_OPERATIONS.contains(&name))
    }

    /// Whether a statement or block does nothing but throw.
    fn only_throws(&self, node: Node<'_>) -> bool {
        match node.kind() {
            "throw_statement" => true,
            "expression_statement" => node
                .named_child(0)
                .is_some_and(|inner| self.is_throwing_call(inner)),
            "statement_block" => {
                let children = named_children(node)
                    .into_iter()
                    .filter(|child| child.kind() != "comment")
                    .collect::<Vec<_>>();
                !children.is_empty() && children.iter().all(|child| self.only_throws(*child))
            }
            _ => false,
        }
    }

    fn if_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let consequence = node.child_by_field_name("consequence")?;
        let alternative = node.child_by_field_name("alternative");
        // A guard that only throws is a precondition. Every implementation
        // states them and no reimplementation does, so keeping them would mean
        // no library behavior ever matched the code that reimplements it.
        if alternative.is_none() && self.only_throws(consequence) {
            return None;
        }
        let condition = node.child_by_field_name("condition")?;
        let condition = self.expression(condition);
        // `if (k in O)` inside a walk of `O` asks whether the element the walk
        // is already visiting exists. An engine must ask because arrays can be
        // sparse; the behavior is the same either way.
        if is_presence_test(&condition) {
            return Some(self.statement(consequence).unwrap_or(Form::Literal));
        }
        Some(Form::Branch {
            condition: Box::new(condition),
            consequence: Box::new(self.statement(consequence).unwrap_or(Form::Literal)),
            alternative: alternative.and_then(|alternative| {
                // `else` on an alternative clause is the clause itself
                let alternative = alternative
                    .named_child(0)
                    .filter(|_| alternative.kind() == "else_clause")
                    .unwrap_or(alternative);
                self.statement(alternative).map(Box::new)
            }),
        })
    }

    /// An index walk is a traversal.
    ///
    /// `for (var k = 0; k < len; k++) { var v = a[k]; .. }` visits each element
    /// of `a` in turn. That is what `for (const v of a)` says and what
    /// `a.forEach(v => ..)` says, and it is the spelling every engine and most
    /// hand-written JavaScript uses. Recognizing it is the single most
    /// important thing this module does.
    fn for_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let body = node.child_by_field_name("body")?;
        let Some((counter, direction)) = self.loop_counter(node) else {
            // not an index walk: keep the shape rather than inventing one
            return Some(Form::Opaque {
                kind: "for".to_owned(),
                parts: vec![self.statement(body).unwrap_or(Form::Literal)],
            });
        };
        // whatever the body indexes with the counter is the sequence
        let indexed = find_indexed(body, &counter, self.source)?;
        let sequence = self.expression(indexed.sequence);
        let item = self.roles.bind(&indexed.binding);
        let Form::Local(index) = item else {
            return None;
        };
        let body = self.loop_body(body, &indexed);
        let body = replace_element_access(&body, &sequence, &counter, &Form::Local(index));
        let body = trim_protocol_arguments(&body, &Form::Local(index), &sequence);
        Some(Form::Traverse {
            sequence: Box::new(sequence),
            item: Box::new(Pattern::Binding(index)),
            body: Box::new(body),
            direction,
        })
    }

    /// The name a `for` header counts with, and which way it runs.
    ///
    /// Counting up from zero walks forwards; counting down while the index
    /// stays at or above zero walks backwards. Both visit every element, and
    /// the difference between them is the difference between `find` and
    /// `findLast`, which the traversal carries so a pattern can see it.
    fn loop_counter(&self, node: Node<'_>) -> Option<(String, Direction)> {
        let initializer = node.child_by_field_name("initializer")?;
        let declarator = descendant(initializer, "variable_declarator")?;
        let name = declarator.child_by_field_name("name")?;
        let value = declarator.child_by_field_name("value")?;
        let starts_at_zero = self.text(value).trim() == "0";
        let condition = node.child_by_field_name("condition")?;
        // the grammar gives the condition directly, except where a bare
        // statement wraps it; unwrapping unconditionally would descend into the
        // comparison and lose the field being tested
        let condition = if condition.kind() == "expression_statement" {
            condition.named_child(0)?
        } else {
            condition
        };
        let counter = self.text(name).to_owned();
        // the counter has to be what the condition bounds and what the update
        // advances, or this is some other loop that happens to look like one
        let bounded = condition
            .child_by_field_name("left")
            .is_some_and(|left| self.text(left) == counter);
        let comparison = condition
            .child_by_field_name("operator")
            .map(|operator| self.text(operator))
            .unwrap_or_default();
        let increment = node.child_by_field_name("increment")?;
        let update = self.text(increment);
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

    /// The loop body with the element access replaced by the item it binds.
    fn loop_body(&mut self, body: Node<'_>, indexed: &Indexed<'_>) -> Form {
        let statements = named_children(body)
            .into_iter()
            .filter(|child| child.kind() != "comment")
            .collect::<Vec<_>>();
        let mut steps = Vec::new();
        for child in statements {
            // the declaration that named the element has already been consumed
            if indexed
                .declaration
                .is_some_and(|declaration| declaration.id() == child.id())
            {
                continue;
            }
            if let Some(step) = self.statement(child) {
                steps.push((step, StatementSpan::of(child)));
            }
        }
        let steps = drop_unused_bindings(inline_aliases(steps));
        let forms = steps.into_iter().map(|(form, _)| form).collect::<Vec<_>>();
        match forms.len() {
            0 => Form::Literal,
            1 => forms.into_iter().next().unwrap_or(Form::Literal),
            _ => Form::Sequence(forms),
        }
    }

    fn for_of_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let sequence = node.child_by_field_name("right")?;
        let sequence = self.expression(sequence);
        let left = node.child_by_field_name("left")?;
        let item = self.pattern(left);
        let body = node.child_by_field_name("body")?;
        let body = self.statement(body).unwrap_or(Form::Literal);
        let body = match &item {
            Pattern::Binding(index) => {
                trim_protocol_arguments(&body, &Form::Local(*index), &sequence)
            }
            _ => body,
        };
        Some(Form::Traverse {
            sequence: Box::new(sequence),
            item: Box::new(item),
            body: Box::new(body),
            direction: Direction::Forward,
        })
    }

    /// A counted `while` is a walk too.
    ///
    /// `while (++index < length) { .. a[index] .. }` is how a great deal of
    /// JavaScript iterates — lodash writes 45 of its files that way — and it is
    /// the same traversal a counted `for` describes, with the step folded into
    /// the test and the initializer sitting in front of the loop. Reading it as
    /// opaque left every library written in that style contributing nothing.
    fn while_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let body = node.child_by_field_name("body")?;
        if let Some((counter, direction)) = self.while_counter(node)
            && let Some(indexed) = find_indexed(body, &counter, self.source)
        {
            let sequence = self.expression(indexed.sequence);
            let item = self.roles.bind(&indexed.binding);
            if let Form::Local(index) = item {
                let walked = self.loop_body(body, &indexed);
                let walked =
                    replace_element_access(&walked, &sequence, &counter, &Form::Local(index));
                let walked = trim_protocol_arguments(&walked, &Form::Local(index), &sequence);
                return Some(Form::Traverse {
                    sequence: Box::new(sequence),
                    item: Box::new(Pattern::Binding(index)),
                    body: Box::new(walked),
                    direction,
                });
            }
        }
        Some(Form::Opaque {
            kind: "while".to_owned(),
            parts: vec![self.statement(body).unwrap_or(Form::Literal)],
        })
    }

    /// The name a `while` header counts with, and which way it runs.
    ///
    /// Either the test advances the counter itself — `while (++k < n)` — or it
    /// compares a counter the body advances. A loop whose counter is never
    /// advanced is not a walk, and is left alone.
    fn while_counter(&self, node: Node<'_>) -> Option<(String, Direction)> {
        let condition = node.child_by_field_name("condition")?;
        let condition = match condition.kind() {
            "parenthesized_expression" => condition.named_child(0)?,
            _ => condition,
        };
        if condition.kind() != "binary_expression" {
            return None;
        }
        let comparison = self.text(condition.child_by_field_name("operator")?);
        let left = condition.child_by_field_name("left")?;
        let body = node.child_by_field_name("body")?;
        // the counter is either advanced in the test or advanced in the body
        let (counter, advanced_here) = match left.kind() {
            "update_expression" => (
                self.text(left.child_by_field_name("argument")?).to_owned(),
                self.text(left).to_owned(),
            ),
            "identifier" => (self.text(left).to_owned(), String::new()),
            _ => return None,
        };
        let advance = if advanced_here.is_empty() {
            let body_text = self.text(body);
            if body_text.contains(&format!("{counter}++"))
                || body_text.contains(&format!("++{counter}"))
            {
                format!("{counter}++")
            } else if body_text.contains(&format!("{counter}--"))
                || body_text.contains(&format!("--{counter}"))
            {
                format!("{counter}--")
            } else {
                return None;
            }
        } else {
            advanced_here
        };
        match comparison {
            "<" | "<=" if advance.contains("++") => Some((counter, Direction::Forward)),
            ">" | ">=" if advance.contains("--") => Some((counter, Direction::Backward)),
            _ => None,
        }
    }

    fn switch_statement(&mut self, node: Node<'_>) -> Form {
        let scrutinee = node
            .child_by_field_name("value")
            .map(|value| self.expression(value))
            .unwrap_or(Form::Literal);
        let mut arms = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            for case in named_children(body) {
                let pattern = match case.child_by_field_name("value") {
                    Some(value) => Pattern::Variant {
                        name: self.text(value).to_owned(),
                        parts: Vec::new(),
                    },
                    None => Pattern::Ignored,
                };
                let mut steps = Vec::new();
                for child in named_children(case) {
                    if child.kind() == "comment"
                        || Some(child.id()) == case.child_by_field_name("value").map(|v| v.id())
                    {
                        continue;
                    }
                    if let Some(step) = self.statement(child) {
                        steps.push(step);
                    }
                }
                arms.push(Arm {
                    pattern,
                    body: match steps.len() {
                        1 => steps.into_iter().next().unwrap_or(Form::Literal),
                        _ => Form::Sequence(steps),
                    },
                });
            }
        }
        Form::select(scrutinee, arms)
    }

    fn declaration(&mut self, node: Node<'_>) -> Option<Form> {
        let mut steps = Vec::new();
        for declarator in named_children(node) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            let name = declarator.child_by_field_name("name")?;
            // the value is normalized before the name is bound, so that
            // `var x = x` reads the outer `x` as the caller wrote it
            let value = match declarator.child_by_field_name("value") {
                Some(value) => self.expression(value),
                // a declaration with no value introduces nothing to compare
                None => {
                    self.roles.bind(self.text(name));
                    continue;
                }
            };
            let pattern = self.pattern(name);
            steps.push(Form::Let {
                pattern: Box::new(pattern),
                value: Box::new(value),
            });
        }
        match steps.len() {
            0 => None,
            1 => steps.into_iter().next(),
            _ => Some(Form::Sequence(steps)),
        }
    }

    fn pattern(&mut self, node: Node<'_>) -> Pattern {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => {
                match self.roles.bind(self.text(node)) {
                    Form::Local(index) => Pattern::Binding(index),
                    _ => Pattern::Ignored,
                }
            }
            "array_pattern" | "object_pattern" => Pattern::Tuple(
                named_children(node)
                    .into_iter()
                    .map(|child| self.pattern(child))
                    .collect(),
            ),
            "variable_declarator" => match node.child_by_field_name("name") {
                Some(name) => self.pattern(name),
                None => Pattern::Ignored,
            },
            _ => match node.named_child(0) {
                Some(inner) => self.pattern(inner),
                None => Pattern::Ignored,
            },
        }
    }

    // -- expressions --------------------------------------------------------

    fn expression(&mut self, node: Node<'_>) -> Form {
        match node.kind() {
            "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
            | "type_assertion" => match node.named_child(0) {
                Some(inner) => self.expression(inner),
                None => Form::Literal,
            },
            "identifier" | "shorthand_property_identifier" => {
                let name = self.text(node);
                // A name in screaming case that nothing here binds is a named
                // constant, and which constant it is *is* behavior:
                // `CreateArrayIterator(this, ITEM_KIND_KEY)` and the same call
                // with `ITEM_KIND_VALUE` are `keys` and `values`. Resolving both
                // to a hole made them one behavior.
                if is_screaming_case(name) && !self.roles.is_value(name) {
                    return Form::Path(name.to_owned());
                }
                self.roles.resolve(name)
            }
            "this" => self.roles.resolve("this"),
            "number" => Form::Number(self.text(node).to_owned()),
            "true" | "false" | "null" => Form::Constant(self.text(node).to_owned()),
            "undefined" => Form::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            },
            "string" | "template_string" => Form::Constant(self.text(node).to_owned()),
            "regex" => Form::Construct("RegExp".to_owned()),
            "array" => {
                // `[...xs]` is a copy of `xs`, and a copy is not behavior. Any
                // other array literal is a construction.
                let children = named_children(node);
                match children.as_slice() {
                    [only] if only.kind() == "spread_element" => match only.named_child(0) {
                        Some(inner) => self.expression(inner),
                        None => Form::Construct("Array".to_owned()),
                    },
                    _ => Form::Construct("Array".to_owned()),
                }
            }
            "object" => Form::Construct("Object".to_owned()),
            "unary_expression" => self.unary(node),
            "binary_expression" => self.binary(node),
            "ternary_expression" => self.ternary(node),
            "call_expression" => self.call(node),
            "new_expression" => self.new_expression(node),
            "member_expression" => self.member(node),
            "subscript_expression" => self.subscript(node),
            "arrow_function" | "function_expression" | "function_declaration" => self.lambda(node),
            "assignment_expression" | "augmented_assignment_expression" => self.assignment(node),
            "update_expression" => match node.named_child(0) {
                Some(inner) => self.expression(inner),
                None => Form::Literal,
            },
            "await_expression" | "spread_element" => match node.named_child(0) {
                Some(inner) => self.expression(inner),
                None => Form::Literal,
            },
            "sequence_expression" => Form::Sequence(
                named_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            ),
            _ => Form::Opaque {
                kind: node.kind().to_owned(),
                parts: named_children(node)
                    .into_iter()
                    .filter(|child| child.kind() != "comment")
                    .map(|child| self.expression(child))
                    .collect(),
            },
        }
    }

    fn unary(&mut self, node: Node<'_>) -> Form {
        let operand = match node.child_by_field_name("argument") {
            Some(operand) => operand,
            None => return Form::Literal,
        };
        let operator = node
            .child_by_field_name("operator")
            .map(|operator| self.text(operator))
            .unwrap_or_default();
        let inner = self.expression(operand);
        match operator {
            // `void 0` is how a specification spells "no value"
            "void" => Form::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            },
            "" => inner,
            _ => Form::Binary {
                operator: operator.to_owned(),
                left: Box::new(inner),
                right: Box::new(Form::Literal),
            },
        }
    }

    fn binary(&mut self, node: Node<'_>) -> Form {
        let operator = node
            .child_by_field_name("operator")
            .map(|operator| self.text(operator).to_owned())
            .unwrap_or_default();
        let left = node
            .child_by_field_name("left")
            .map(|left| self.expression(left))
            .unwrap_or(Form::Literal);
        let right = node
            .child_by_field_name("right")
            .map(|right| self.expression(right))
            .unwrap_or(Form::Literal);
        Form::Binary {
            // `==` and `===` differ in coercion, not in what is being asked
            operator: match operator.as_str() {
                "===" => "==".to_owned(),
                "!==" => "!=".to_owned(),
                other => other.to_owned(),
            },
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    fn ternary(&mut self, node: Node<'_>) -> Form {
        Form::Branch {
            condition: Box::new(
                node.child_by_field_name("condition")
                    .map(|condition| self.expression(condition))
                    .unwrap_or(Form::Literal),
            ),
            consequence: Box::new(
                node.child_by_field_name("consequence")
                    .map(|consequence| self.expression(consequence))
                    .unwrap_or(Form::Literal),
            ),
            alternative: node
                .child_by_field_name("alternative")
                .map(|alternative| Box::new(self.expression(alternative))),
        }
    }

    fn call(&mut self, node: Node<'_>) -> Form {
        let Some(callee) = node.child_by_field_name("function") else {
            return Form::Literal;
        };
        let arguments = node
            .child_by_field_name("arguments")
            .map(|arguments| {
                named_children(arguments)
                    .into_iter()
                    .filter(|argument| argument.kind() != "comment")
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if callee.kind() == "identifier" {
            let name = self.text(callee);
            // a coercion returns what it was given
            if IDENTITY_OPERATIONS.contains(&name) {
                return match arguments.first() {
                    Some(first) => self.expression(*first),
                    None => Form::Literal,
                };
            }
            // `callContentFunction(f, thisArg, ..args)` is `f(..args)`
            if THIS_ARG_CALLS.contains(&name) && arguments.len() >= 2 {
                let function = self.expression(arguments[0]);
                let rest = arguments[2..]
                    .iter()
                    .map(|argument| self.expression(*argument))
                    .collect();
                return Form::Call {
                    callee: Box::new(function),
                    arguments: rest,
                };
            }
            if PRECONDITION_OPERATIONS.contains(&name) {
                return Form::Opaque {
                    kind: "precondition".to_owned(),
                    parts: Vec::new(),
                };
            }
        }

        if callee.kind() == "member_expression" {
            return self.method_call(callee, &arguments);
        }

        // Calling a function defined elsewhere is not the same as calling one
        // the caller supplied. `helper(x)` names something a reader can go and
        // look at, and two delegations to different helpers are two behaviors;
        // resolving the callee to a hole made every one-line delegation alike.
        let function = match callee.kind() {
            "identifier" if !self.roles.is_value(self.text(callee)) => {
                Form::Path(self.text(callee).to_owned())
            }
            _ => self.expression(callee),
        };
        Form::Call {
            callee: Box::new(function),
            arguments: arguments
                .iter()
                .map(|argument| self.expression(*argument))
                .collect(),
        }
    }

    /// A method call, with the sequence operations given their own forms.
    fn method_call(&mut self, callee: Node<'_>, arguments: &[Node<'_>]) -> Form {
        let Some(property) = callee.child_by_field_name("property") else {
            return Form::Literal;
        };
        let name = self.text(property).to_owned();
        let Some(object) = callee.child_by_field_name("object") else {
            return Form::Literal;
        };

        // `f.call(thisArg, ..args)` is `f(..args)`
        if THIS_ARG_CALLS.contains(&name.as_str()) && !arguments.is_empty() {
            let function = self.expression(object);
            let rest = arguments[1..]
                .iter()
                .map(|argument| self.expression(*argument))
                .collect();
            return Form::Call {
                callee: Box::new(function),
                arguments: rest,
            };
        }
        // `f.apply(thisArg, [a, b])` is `f(a, b)`
        if name == "apply" && arguments.len() == 2 && arguments[1].kind() == "array" {
            let function = self.expression(object);
            let rest = named_children(arguments[1])
                .into_iter()
                .map(|argument| self.expression(argument))
                .collect();
            return Form::Call {
                callee: Box::new(function),
                arguments: rest,
            };
        }

        let sequence = self.expression(object);
        if SEQUENCE_ADAPTERS.contains(&name.as_str()) && arguments.is_empty() {
            return sequence;
        }
        // the operations that describe iteration get the forms that describe it
        let described = match name.as_str() {
            "filter" => Some(SequenceOperation::Retain),
            "map" => Some(SequenceOperation::Transform),
            "forEach" => Some(SequenceOperation::Traverse),
            "flatMap" => Some(SequenceOperation::Sift),
            _ => None,
        };
        // the second argument, where there is one, is the receiver to call the
        // callback on; it changes who `this` is and not what is iterated
        if let Some(operation) = described
            && let [callback] | [callback, _] = arguments
            && let Some((item, body)) = self.callback(*callback)
        {
            return operation.build(sequence, item, body);
        }
        // `xs.reverse().find(p)` walks from the back, which is `findLast`.
        //
        // Reading it as a plain `find` would say the code already calls the
        // library it should — the opposite of the truth. Rewriting it into
        // "the last thing that passed" lets the ordinary law do the rest, and
        // a `find` over an unreversed sequence is left exactly as it was.
        if name == "find"
            && let [callback] | [callback, _] = arguments
            && let Form::Method {
                name: adapter,
                receiver: reversed,
                arguments: none,
            } = &sequence
            && adapter == "reverse"
            && none.is_empty()
            && let Some((item, body)) = self.callback(*callback)
        {
            return Form::Method {
                name: "last".to_owned(),
                receiver: Box::new(Form::Retain {
                    sequence: reversed.clone(),
                    item: Box::new(item),
                    body: Box::new(body),
                }),
                arguments: Vec::new(),
            };
        }

        // Taking one end of a sequence, however it is spelled.
        //
        // `a[0]`, `a.at(0)` and `a.shift()` all take the first; `a.pop()` and
        // `a.at(-1)` all take the last. Which END is behavior — searching
        // forwards and searching backwards are different operations, and the
        // reverse iterator outranking the forward one is a mistake this
        // codebase has already paid for once.
        if let Some(end) = self.sequence_end(&name, arguments) {
            return Form::Method {
                name: end.to_owned(),
                receiver: Box::new(sequence),
                arguments: Vec::new(),
            };
        }
        if name == "reduce"
            && let [callback, initial] = arguments
            && let Some((accumulator, item, body)) = self.reducer(*callback)
        {
            let initial = self.expression(*initial);
            return Form::Accumulate {
                sequence: Box::new(sequence),
                initial: Box::new(initial),
                accumulator: Box::new(accumulator),
                item: Box::new(item),
                body: Box::new(body),
            };
        }
        Form::Method {
            name,
            receiver: Box::new(sequence),
            arguments: arguments
                .iter()
                .map(|argument| self.expression(*argument))
                .collect(),
        }
    }

    /// Which end of a sequence a method takes, when it takes one.
    ///
    /// Returns the canonical name for that end, so that every spelling of
    /// "the first one" reduces to one form and stays distinct from "the last
    /// one". An index that is not an end — `at(1)`, `at(n)` — is not this.
    fn sequence_end(&self, name: &str, arguments: &[Node<'_>]) -> Option<&'static str> {
        match (name, arguments) {
            ("shift", []) => Some("first"),
            ("pop", []) => Some("last"),
            ("at", [index]) => match self.text(*index).trim() {
                "0" => Some("first"),
                "-1" => Some("last"),
                _ => None,
            },
            _ => None,
        }
    }

    /// A callback's item binding and body, when it is written as a function.
    fn callback(&mut self, node: Node<'_>) -> Option<(Pattern, Form)> {
        // A function passed by name describes the same traversal as a closure
        // that calls it: `filter(isReady)` and `filter(x => isReady(x))` differ
        // in spelling only. Only the closure gives the element a name, so
        // normalizing them alike means supplying one.
        // `filter(...foo)` spreads an argument list whose shape is unknown, so
        // there is no callback here and no traversal to describe
        if node.kind() == "spread_element" {
            return None;
        }
        if !matches!(node.kind(), "arrow_function" | "function_expression") {
            let function = self.expression(node);
            let Form::Local(index) = self.roles.bind_anonymous() else {
                return None;
            };
            return Some((
                Pattern::Binding(index),
                Form::Call {
                    callee: Box::new(function),
                    arguments: vec![Form::Local(index)],
                },
            ));
        }
        let parameters = node
            .child_by_field_name("parameter")
            .or_else(|| node.child_by_field_name("parameters"))?;
        let first = if parameters.kind() == "identifier" {
            parameters
        } else {
            named_children(parameters).into_iter().next()?
        };
        let item = self.pattern(first);
        let body = node.child_by_field_name("body")?;
        let body = match body.kind() {
            "statement_block" => self.block(body),
            _ => self.expression(body),
        };
        Some((item, body))
    }

    fn reducer(&mut self, node: Node<'_>) -> Option<(Pattern, Pattern, Form)> {
        if !matches!(node.kind(), "arrow_function" | "function_expression") {
            return None;
        }
        let parameters = node.child_by_field_name("parameters")?;
        let mut children = named_children(parameters).into_iter();
        let accumulator = self.pattern(children.next()?);
        let item = self.pattern(children.next()?);
        let body = node.child_by_field_name("body")?;
        let body = match body.kind() {
            "statement_block" => self.block(body),
            _ => self.expression(body),
        };
        Some((accumulator, item, body))
    }

    fn new_expression(&mut self, node: Node<'_>) -> Form {
        match node.child_by_field_name("constructor") {
            Some(constructor) => Form::Construct(
                self.text(constructor)
                    .rsplit('.')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            None => Form::Literal,
        }
    }

    fn member(&mut self, node: Node<'_>) -> Form {
        let Some(object) = node.child_by_field_name("object") else {
            return Form::Literal;
        };
        let value = self.expression(object);
        let name = node
            .child_by_field_name("property")
            .map(|property| self.text(property).to_owned())
            .unwrap_or_default();
        Form::Field {
            value: Box::new(value),
            name,
        }
    }

    fn subscript(&mut self, node: Node<'_>) -> Form {
        let Some(object) = node.child_by_field_name("object") else {
            return Form::Literal;
        };
        let value = self.expression(object);
        let name = node
            .child_by_field_name("index")
            .map(|index| self.text(index).to_owned())
            .unwrap_or_default();
        // `a[0]` and `a['0']` take the first element, the same thing `a.at(0)`
        // and `a.shift()` do. Which end is behavior; `a[1]` is not an end.
        //
        // Written as the target of an assignment it is not taking anything at
        // all — `xs.filter(p)[0] += 1` names a place to store into, and a
        // search yields a value that has no place. Reading the two alike would
        // report a rewrite that cannot be made.
        if name.trim().trim_matches(['\'', '"', '`']) == "0" && !is_assigned_to(node, self.source) {
            return Form::Method {
                name: "first".to_owned(),
                receiver: Box::new(value),
                arguments: Vec::new(),
            };
        }
        Form::Field {
            value: Box::new(value),
            name,
        }
    }

    fn lambda(&mut self, node: Node<'_>) -> Form {
        let parameters = node
            .child_by_field_name("parameter")
            .or_else(|| node.child_by_field_name("parameters"));
        let patterns = match parameters {
            Some(parameters) if parameters.kind() == "identifier" => vec![self.pattern(parameters)],
            Some(parameters) => named_children(parameters)
                .into_iter()
                .map(|parameter| self.pattern(parameter))
                .collect(),
            None => Vec::new(),
        };
        let body = match node.child_by_field_name("body") {
            Some(body) if body.kind() == "statement_block" => self.block(body),
            Some(body) => self.expression(body),
            None => Form::Literal,
        };
        Form::Lambda {
            parameters: patterns,
            body: Box::new(body),
        }
    }

    fn assignment(&mut self, node: Node<'_>) -> Form {
        let operator = node
            .child_by_field_name("operator")
            .map(|operator| self.text(operator).to_owned())
            .unwrap_or_else(|| "=".to_owned());
        let target = node
            .child_by_field_name("left")
            .map(|left| self.expression(left))
            .unwrap_or(Form::Literal);
        let value = node
            .child_by_field_name("right")
            .map(|right| self.expression(right))
            .unwrap_or(Form::Literal);
        Form::Assign {
            operator,
            target: Box::new(target),
            value: Box::new(value),
        }
    }
}

/// Which iteration form a sequence method describes.
enum SequenceOperation {
    Retain,
    Transform,
    Traverse,
    Sift,
}

impl SequenceOperation {
    fn build(self, sequence: Form, item: Pattern, body: Form) -> Form {
        let sequence = Box::new(sequence);
        let item = Box::new(item);
        let body = Box::new(body);
        match self {
            Self::Retain => Form::Retain {
                sequence,
                item,
                body,
            },
            Self::Transform => Form::Transform {
                sequence,
                item,
                body,
            },
            Self::Traverse => Form::Traverse {
                sequence,
                item,
                body,
                direction: Direction::Forward,
            },
            Self::Sift => Form::Sift {
                sequence,
                item,
                body,
            },
        }
    }
}

/// Whether an expression names a place being written to rather than a value.
///
/// Assignment, compound assignment, `++`/`--`, `delete`, and a destructuring
/// target all use an expression for where to put something. What they name has
/// to be a place, and the result of a search is not one.
fn is_assigned_to(node: Node<'_>, source: &[u8]) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "assignment_expression" | "augmented_assignment_expression" => parent
            .child_by_field_name("left")
            .is_some_and(|left| left.id() == node.id()),
        "update_expression" => true,
        // `delete a[0]` removes the place; `!a[0]` merely reads it
        "unary_expression" => parent
            .child_by_field_name("operator")
            .is_some_and(|operator| text(operator, source) == "delete"),
        "array_pattern" | "object_pattern" => true,
        _ => false,
    }
}

/// Whether a name is written the way a constant is written.
fn is_screaming_case(name: &str) -> bool {
    name.len() > 1
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
        && name.chars().any(|character| character.is_ascii_uppercase())
}

/// An element access found in a loop body, and what names it.
struct Indexed<'a> {
    sequence: Node<'a>,
    binding: String,
    /// The declaration that gave the element a name, when there is one.
    declaration: Option<Node<'a>>,
}

/// The sequence a loop body indexes with `counter`, and the name it binds.
///
/// A body normally names the element first — `var v = a[k];` — and then works
/// with the name. When it does not, the access itself is the element and a name
/// is supplied, because a traversal has to bind one.
fn find_indexed<'a>(body: Node<'a>, counter: &str, source: &[u8]) -> Option<Indexed<'a>> {
    let mut found = None;
    fn walk<'a>(node: Node<'a>, counter: &str, source: &[u8], found: &mut Option<Node<'a>>) {
        if found.is_some() {
            return;
        }
        if node.kind() == "subscript_expression"
            && node
                .child_by_field_name("index")
                .is_some_and(|index| text(index, source) == counter)
        {
            *found = Some(node);
            return;
        }
        for child in named_children(node) {
            walk(child, counter, source, found);
        }
    }
    walk(body, counter, source, &mut found);
    let access = found?;
    let sequence = access.child_by_field_name("object")?;

    // was the access immediately given a name?
    let mut declaration = None;
    let mut binding = None;
    for statement in named_children(body) {
        if !matches!(
            statement.kind(),
            "variable_declaration" | "lexical_declaration"
        ) {
            continue;
        }
        for declarator in named_children(statement) {
            let Some(value) = declarator.child_by_field_name("value") else {
                continue;
            };
            if value.id() == access.id()
                && let Some(name) = declarator.child_by_field_name("name")
            {
                binding = Some(text(name, source).to_owned());
                declaration = Some(statement);
            }
        }
    }
    Some(Indexed {
        sequence,
        binding: binding.unwrap_or_else(|| format!("__item_{counter}")),
        declaration,
    })
}

/// The first descendant of `kind`, including the node itself.
fn descendant<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    named_children(node)
        .into_iter()
        .find_map(|child| descendant(child, kind))
}

/// Normalize one function, declaring its parameters before its body.
/// Declare a function's parameters, and its receiver, as values.
///
/// Without them every bare call looks alike, and a function a reader could go
/// and look at cannot be told from an argument the caller supplied.
fn declare_parameters(function: Node<'_>, source: &[u8], normalizer: &mut Normalizer<'_>) {
    if let Some(parameters) = function
        .child_by_field_name("parameters")
        .or_else(|| function.child_by_field_name("parameter"))
    {
        if parameters.kind() == "identifier" {
            normalizer.roles.declare(text(parameters, source));
        }
        for parameter in named_children(parameters) {
            let name = match parameter.kind() {
                "identifier" => Some(parameter),
                _ => parameter
                    .child_by_field_name("pattern")
                    .or_else(|| parameter.named_child(0)),
            };
            if let Some(name) = name.filter(|name| name.kind() == "identifier") {
                normalizer.roles.declare(text(name, source));
            }
        }
    }
    // the receiver is data the caller supplied, exactly like a parameter
    normalizer.roles.declare("this");
}

pub fn normalize_function(function: Node<'_>, source: &[u8]) -> Form {
    let mut normalizer = Normalizer::new(source);
    declare_parameters(function, source, &mut normalizer);
    match function.child_by_field_name("body") {
        Some(body) if body.kind() == "statement_block" => valued(normalizer.block(body)),
        Some(body) => valued(normalizer.expression(body)),
        None => Form::Sequence(Vec::new()),
    }
}

/// Normalize one function, keeping where each step of the body was written.
pub fn normalize_function_located(
    function: Node<'_>,
    source: &[u8],
) -> (Form, Vec<StatementSpan>, Vec<LocatedForm>) {
    let mut normalizer = Normalizer::new(source);
    declare_parameters(function, source, &mut normalizer);
    let (form, spans) = match function.child_by_field_name("body") {
        Some(body) if body.kind() == "statement_block" => {
            let (form, spans) = normalizer.block_located(body);
            (valued(form), spans)
        }
        Some(body) => (valued(normalizer.expression(body)), Vec::new()),
        None => (Form::Sequence(Vec::new()), Vec::new()),
    };
    (form, spans, normalizer.located)
}

/// Normalize a bare body, with no parameters in scope.
pub fn normalize_body(body: Node<'_>, source: &[u8]) -> Form {
    let mut normalizer = Normalizer::new(source);
    normalizer.block(body)
}

/// Normalize a file's top-level statements as one body.
///
/// A module is code too. JavaScript and TypeScript both let work sit at the top
/// level of a file rather than inside a function, and a normalizer that only
/// looked at functions would report nothing about it — silently, which is the
/// one failure mode worth going out of the way to avoid.
pub fn normalize_module(file: &ParsedFile) -> Form {
    let root = file.tree.root_node();
    let mut normalizer = Normalizer::new(&file.source);
    valued(normalizer.block(root))
}

/// Every named function, however the language spells one.
///
/// `function f() {}`, a class method, and `const f = () => {}` all define a
/// named function, and TypeScript uses the third constantly. Collecting only
/// the first two left roughly a quarter of a real project's named functions
/// unread — and unread reads downstream as "nothing there", which is the
/// failure worth going out of the way to avoid.
fn collect_functions<'a>(node: Node<'a>, output: &mut Vec<Node<'a>>) {
    if matches!(node.kind(), "function_declaration" | "method_definition") {
        output.push(node);
        return;
    }
    // `const f = () => ..` binds a function to a name; the declarator is what
    // carries the name, and the arrow is what carries the body.
    if node.kind() == "variable_declarator"
        && node
            .child_by_field_name("value")
            .is_some_and(|value| matches!(value.kind(), "arrow_function" | "function_expression"))
        && node
            .child_by_field_name("name")
            .is_some_and(|name| name.kind() == "identifier")
    {
        output.push(node);
        return;
    }
    for child in named_children(node) {
        collect_functions(child, output);
    }
}

/// Normalize every function in a parsed TypeScript or JavaScript file.
pub fn normalize_file(file: &ParsedFile) -> Vec<NormalizedFunction> {
    let mut nodes = Vec::new();
    collect_functions(file.tree.root_node(), &mut nodes);
    nodes
        .into_iter()
        .filter_map(|node| {
            let name = node.child_by_field_name("name")?;
            // a declarator names the binding and the arrow holds the body
            let definition = match node.kind() {
                "variable_declarator" => node.child_by_field_name("value")?,
                _ => node,
            };
            // a signature with no body declares behavior rather than describing it
            definition.child_by_field_name("body")?;
            let located = normalize_function_located(definition, &file.source);
            Some(NormalizedFunction {
                name: text(name, &file.source).to_owned(),
                start_byte: node.start_byte() as u64,
                end_byte: node.end_byte() as u64,
                start_line: u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX),
                end_line: u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX),
                form: located.0,
                statements: located.1,
                located: located.2,
                damaged: node.has_error(),
            })
        })
        .collect()
}
