//! Normalizes Python syntax into Infact's behavior form.
//!
//! This module knows Python and nothing else. It names no package, no callable,
//! and no API: every rule here is about how the language spells something, so
//! one implementation covers every library a repository depends on.
//!
//! The canonicalizations, in order of how much they matter:
//!
//! 1. **Building a sequence.** Python writes "make a new list from an old one"
//!    three ways — a comprehension, a generator expression, and three
//!    statements that bind an empty list and append to it in a loop. All three
//!    reduce to a `Collect` over a `Transform`. This is Python's equivalent of
//!    the rule that an index walk and a `for..of` are one traversal, and
//!    without it nothing else here is worth anything.
//! 2. **Failure.** Python spells a fallible operation with `try`/`except`,
//!    where Rust spells it with a `Result` and a `match`. Both become a
//!    `Select` whose arms name what went wrong, so the two meet.
//! 3. **Preconditions.** A guard that only raises, and an `assert`, state what
//!    the caller must not do. Every implementation has them and no caller
//!    reimplementing the behavior writes them, so they are not behavior.
//! 4. **Syntactic noise.** Type annotations, decorators, parentheses, `pass`,
//!    `global`, imports, and the adapters that hand back the sequence they were
//!    given.
//!
//! # What is deliberately not here
//!
//! No library is named. `sum`, `sorted`, `max` and the rest stay ordinary calls
//! rather than being folded into the loops they stand for: which builtin was
//! called is behavior, and a frontend that reduced them would say a caller had
//! reimplemented what it in fact called. Rust and TypeScript recognize their
//! iteration METHODS because those take a closure whose body is the behavior;
//! Python's builtins take a whole sequence and are opaque.

mod cleanup;

use std::collections::BTreeMap;

use cleanup::{drop_unused_bindings, fuse_container_fills, inline_aliases, push_flattened, valued};
use entl_tree_sitter::ParsedFile;
use infact_normalize::{Arm, Direction, Form, Pattern, Roles};
use tree_sitter::Node;

/// Calls that hand back the sequence they were given.
///
/// Only in the position a traversal reads its sequence from. `list(xs)` is a
/// copy when it stands alone and is nothing at all when it is what a `for`
/// walks, and peeling the standalone one would erase a real allocation.
const SEQUENCE_ADAPTERS: &[&str] = &["iter", "list", "tuple"];

/// Calls that walk a sequence backwards.
const REVERSING_ADAPTERS: &[&str] = &["reversed"];

/// Calls that gather a sequence into a named container.
const CONTAINERS: &[&str] = &["list", "set", "tuple", "dict", "frozenset"];

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
    /// Aligned with the steps by construction rather than by recounting the
    /// body's statements: normalization drops some outright — a guard that only
    /// raises, a binding nothing reads, an import — so counting source
    /// statements separately would slide the spans against the steps and report
    /// a match at the wrong line.
    pub statements: Vec<StatementSpan>,
    /// Every statement inside this function, at every depth, with its own form
    /// and span. This is what lets a match be reported at the line that carries
    /// it rather than at the function that contains it.
    pub located: Vec<LocatedForm>,
    /// Whether the grammar failed anywhere inside this function.
    ///
    /// A consumer should decline the callable rather than derive a behavior
    /// from a body it only partly understood.
    pub damaged: bool,
}

/// One statement, its normalized form, and where it was written.
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

/// Whether a name is spelled the way Python spells a module-level constant.
///
/// `MAX_SIZE` and `_DEFAULT` qualify; `Transport` and `x` do not. A single
/// upper-case letter is excluded because `T` is overwhelmingly a type variable
/// rather than a constant, and a type variable is not behavior.
fn is_screaming_case(name: &str) -> bool {
    name.len() > 1
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
        && name.chars().any(|character| character.is_ascii_uppercase())
}

/// Children that carry code, which is every named child but a comment.
fn code_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    named_children(node)
        .into_iter()
        .filter(|child| !matches!(child.kind(), "comment" | "line_continuation"))
        .collect()
}

/// The names a file imported, and the module each names.
///
/// `from json.decoder import JSONDecoder` makes `JSONDecoder` mean
/// `json.decoder.JSONDecoder` in this file and nowhere else, so two packages
/// that each import a different `Regex` stop being one form. The statement
/// says which module it read, so this needs no filesystem and no other file.
///
/// Only imports are qualified. A class defined in the file it is used in stays
/// bare, because naming its module would need the package layout on disk, and
/// measuring said the whole of qualification is worth at most 23% of the
/// remaining ambiguity — not enough to make the normalizer read directories.
type Imports = BTreeMap<String, String>;

fn imports_of(root: Node<'_>, source: &[u8]) -> Imports {
    let mut imports = Imports::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_from_statement" => {
                let module = node
                    .child_by_field_name("module_name")
                    .map(|module| text(module, source).to_owned())
                    .unwrap_or_default();
                let mut cursor = node.walk();
                for imported in node.children_by_field_name("name", &mut cursor) {
                    let (origin, local) = alias(imported, source);
                    imports.insert(local, format!("{module}.{origin}"));
                }
            }
            "import_statement" => {
                let mut cursor = node.walk();
                for imported in node.children_by_field_name("name", &mut cursor) {
                    let (origin, local) = alias(imported, source);
                    // `import a.b.c` binds `a`; `import a.b as c` binds `c`.
                    match imported.kind() {
                        "aliased_import" => imports.insert(local, origin),
                        _ => {
                            let head = origin.split('.').next().unwrap_or(&origin).to_owned();
                            imports.insert(head.clone(), head)
                        }
                    };
                }
            }
            _ => {}
        }
        for child in named_children(node) {
            stack.push(child);
        }
    }
    imports
}

/// What an import names, and what this file calls it.
fn alias(node: Node<'_>, source: &[u8]) -> (String, String) {
    match node.kind() {
        "aliased_import" => {
            let origin = node
                .child_by_field_name("name")
                .map(|name| text(name, source).to_owned())
                .unwrap_or_default();
            let local = node
                .child_by_field_name("alias")
                .map(|name| text(name, source).to_owned())
                .unwrap_or_else(|| origin.clone());
            (origin, local)
        }
        _ => {
            let name = text(node, source).to_owned();
            (name.clone(), name)
        }
    }
}

/// The tree a node belongs to.
fn root_of(node: Node<'_>) -> Node<'_> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        current = parent;
    }
    current
}

struct Normalizer<'a> {
    source: &'a [u8],
    imports: &'a Imports,
    roles: Roles,
    located: Vec<LocatedForm>,
    depth: u32,
}

impl<'a> Normalizer<'a> {
    fn new(source: &'a [u8], imports: &'a Imports) -> Self {
        Self {
            source,
            imports,
            roles: Roles::new(),
            located: Vec::new(),
            depth: 0,
        }
    }

    /// What a name means outside this body, qualified when the file says so.
    fn path(&self, name: &str) -> Form {
        Form::Path(
            self.imports
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_owned()),
        )
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
        for child in code_children(node) {
            if let Some(step) = self.statement(child) {
                push_flattened(&mut steps, step, StatementSpan::of(child));
            }
        }
        let steps = drop_unused_bindings(fuse_container_fills(inline_aliases(steps)));
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
    fn statement(&mut self, node: Node<'_>) -> Option<Form> {
        let form = self.statement_form(node)?;
        if node.kind() != "block" && !form.is_trivial() {
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
            // Declarations and scope directives. An import binds a name and
            // does no work; which module a body imported is a fact about the
            // file, and this crate reports behavior.
            "comment"
            | "pass_statement"
            | "import_statement"
            | "import_from_statement"
            | "future_import_statement"
            | "global_statement"
            | "nonlocal_statement"
            | "type_alias_statement"
            | "line_continuation" => None,
            // A precondition, not work. `assert` also vanishes under `-O`,
            // which makes treating it as behavior wrong twice over.
            "assert_statement" => None,
            // Abandoning the computation is not what the computation does.
            "raise_statement" => None,
            "expression_statement" => {
                let inner = code_children(node);
                match inner.as_slice() {
                    [] => None,
                    [only] => Some(self.expression(*only)),
                    many => Some(Form::Sequence(
                        many.iter().map(|child| self.expression(*child)).collect(),
                    )),
                }
            }
            "return_statement" => Some(Form::Return(Box::new(
                match code_children(node).first().copied() {
                    Some(value) => self.expression(value),
                    None => Form::Literal,
                },
            ))),
            "block" => Some(self.block(node)),
            "if_statement" => self.if_statement(node),
            "for_statement" => self.for_statement(node),
            "while_statement" => Some(self.while_statement(node)),
            "with_statement" => Some(self.with_statement(node)),
            "try_statement" => Some(self.try_statement(node)),
            "match_statement" => Some(self.match_statement(node)),
            "delete_statement" => Some(Form::Method {
                name: "delete".to_owned(),
                receiver: Box::new(match code_children(node).first().copied() {
                    Some(target) => self.expression(target),
                    None => Form::Literal,
                }),
                arguments: Vec::new(),
            }),
            "break_statement" => Some(Form::Variant {
                name: "Break".to_owned(),
                payload: Vec::new(),
            }),
            "continue_statement" => Some(Form::Variant {
                name: "Continue".to_owned(),
                payload: Vec::new(),
            }),
            // A nested definition is a value the surrounding body produced.
            "function_definition" | "class_definition" => {
                let name = node.child_by_field_name("name")?;
                let bound = self.roles.bind(self.text(name));
                let value = if node.kind() == "function_definition" {
                    self.lambda(node)
                } else {
                    Form::Construct(self.text(name).to_owned())
                };
                Some(Form::Let {
                    pattern: Box::new(match bound {
                        Form::Local(index) => Pattern::Binding(index),
                        _ => Pattern::Ignored,
                    }),
                    value: Box::new(value),
                })
            }
            // A decorator annotates; the definition is the behavior.
            "decorated_definition" => {
                let definition = node.child_by_field_name("definition")?;
                self.statement_form(definition)
            }
            _ => Some(self.expression(node)),
        }
    }

    /// Whether a statement or block does nothing but raise.
    fn only_raises(&self, node: Node<'_>) -> bool {
        match node.kind() {
            "raise_statement" => true,
            "block" => {
                let children = code_children(node);
                !children.is_empty() && children.iter().all(|child| self.only_raises(*child))
            }
            _ => false,
        }
    }

    fn if_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let consequence = node.child_by_field_name("consequence")?;
        let alternatives = named_children(node)
            .into_iter()
            .filter(|child| matches!(child.kind(), "elif_clause" | "else_clause"))
            .collect::<Vec<_>>();
        // A guard that only raises is a precondition. Every implementation
        // states them and no reimplementation does, so keeping them would mean
        // no library behavior ever matched the code that reimplements it.
        if alternatives.is_empty() && self.only_raises(consequence) {
            return None;
        }
        let condition = node.child_by_field_name("condition")?;
        let condition = self.expression(condition);
        Some(Form::Branch {
            condition: Box::new(condition),
            consequence: Box::new(self.statement(consequence).unwrap_or(Form::Literal)),
            alternative: self.else_chain(&alternatives).map(Box::new),
        })
    }

    /// `elif` is `else: if`, and holding it as anything else would make one
    /// decision written two ways into two forms.
    fn else_chain(&mut self, clauses: &[Node<'_>]) -> Option<Form> {
        let (first, rest) = clauses.split_first()?;
        match first.kind() {
            "elif_clause" => {
                let condition = first.child_by_field_name("condition")?;
                let consequence = first.child_by_field_name("consequence")?;
                let condition = self.expression(condition);
                Some(Form::Branch {
                    condition: Box::new(condition),
                    consequence: Box::new(self.statement(consequence).unwrap_or(Form::Literal)),
                    alternative: self.else_chain(rest).map(Box::new),
                })
            }
            _ => {
                let body = first
                    .child_by_field_name("body")
                    .or_else(|| code_children(*first).first().copied())?;
                self.statement(body)
            }
        }
    }

    /// A `for` is a traversal, which is the form's whole reason to exist.
    fn for_statement(&mut self, node: Node<'_>) -> Option<Form> {
        let left = node.child_by_field_name("left")?;
        let right = node.child_by_field_name("right")?;
        let body = node.child_by_field_name("body")?;
        let (sequence, direction) = self.sequence(right);
        let item = self.bind_pattern(left);
        let walked = self.statement(body).unwrap_or(Form::Literal);
        let traverse = Form::Traverse {
            sequence: Box::new(sequence),
            item: Box::new(item),
            body: Box::new(walked),
            direction,
        };
        // A `for..else` runs the else only when the walk was not broken out of.
        let Some(otherwise) = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "else_clause")
        else {
            return Some(traverse);
        };
        let Some(otherwise) = self.else_chain(&[otherwise]) else {
            return Some(traverse);
        };
        Some(Form::Sequence(vec![traverse, otherwise]))
    }

    /// A sequence being walked, with the adapters that only say how peeled off.
    fn sequence(&mut self, node: Node<'_>) -> (Form, Direction) {
        if node.kind() == "call"
            && let Some(callee) = node.child_by_field_name("function")
            && callee.kind() == "identifier"
            && let Some(arguments) = node.child_by_field_name("arguments")
            && arguments.kind() == "argument_list"
            && let [only] = code_children(arguments).as_slice()
        {
            let name = self.text(callee);
            if SEQUENCE_ADAPTERS.contains(&name) && !self.roles.is_value(name) {
                return self.sequence(*only);
            }
            if REVERSING_ADAPTERS.contains(&name) && !self.roles.is_value(name) {
                // Which way a walk runs is behavior, and a form that lost it
                // would report a forward search where the code searched back.
                let (sequence, _) = self.sequence(*only);
                return (sequence, Direction::Backward);
            }
        }
        (self.expression(node), Direction::Forward)
    }

    fn while_statement(&mut self, node: Node<'_>) -> Form {
        // A `while` walks something the syntax does not name. Held opaque
        // rather than guessed at, so the gap stays visible instead of turning
        // every loop into the same traversal.
        let condition = node
            .child_by_field_name("condition")
            .map_or(Form::Literal, |condition| self.expression(condition));
        let body = node
            .child_by_field_name("body")
            .and_then(|body| self.statement(body))
            .unwrap_or(Form::Literal);
        Form::Opaque {
            kind: "while".to_owned(),
            parts: vec![condition, body],
        }
    }

    /// `with open(p) as f: f.read()` binds `f` and then works with it.
    ///
    /// Reducing it to the binding and the body is what lets it meet the same
    /// work written without a context manager. What the manager does on the way
    /// out is real, and it is also invisible to the syntax.
    fn with_statement(&mut self, node: Node<'_>) -> Form {
        let mut steps = Vec::new();
        for clause in named_children(node) {
            if clause.kind() != "with_clause" {
                continue;
            }
            for item in code_children(clause) {
                let Some(value) = item.child_by_field_name("value") else {
                    continue;
                };
                match self.as_pattern(value) {
                    Some((subject, alias)) => {
                        let value = self.expression(subject);
                        let pattern = self.bind_pattern(alias);
                        steps.push(Form::Let {
                            pattern: Box::new(pattern),
                            value: Box::new(value),
                        });
                    }
                    None => steps.push(self.expression(value)),
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body")
            && let Some(form) = self.statement(body)
        {
            steps.push(form);
        }
        match steps.len() {
            1 => steps.into_iter().next().unwrap_or(Form::Literal),
            _ => Form::Sequence(steps),
        }
    }

    /// The subject and the name of an `X as y`, when that is what a node is.
    fn as_pattern<'b>(&self, node: Node<'b>) -> Option<(Node<'b>, Node<'b>)> {
        if node.kind() != "as_pattern" {
            return None;
        }
        let alias = node.child_by_field_name("alias")?;
        let subject = code_children(node)
            .into_iter()
            .find(|child| child.kind() != "as_pattern_target")?;
        let bound = code_children(alias).first().copied().unwrap_or(alias);
        Some((subject, bound))
    }

    /// `try`/`except` is how Python spells a fallible operation.
    ///
    /// Rust spells the same thing with a `Result` and a `match`, and this puts
    /// the two into one shape: the work is the scrutinee and each handler is an
    /// arm naming what it catches. WHICH exception is caught is behavior —
    /// recovering from a missing key and recovering from a broken socket are
    /// not the same operation — so the type name is kept.
    fn try_statement(&mut self, node: Node<'_>) -> Form {
        let body = node
            .child_by_field_name("body")
            .map_or(Form::Literal, |body| self.block(body));
        let mut arms = Vec::new();
        let mut after = Vec::new();
        for clause in named_children(node) {
            match clause.kind() {
                "except_clause" | "except_group_clause" => arms.push(self.except_arm(clause)),
                // A `try..else` runs when nothing was raised, which is the
                // success path and belongs with the work.
                "else_clause" => {
                    if let Some(form) = self.else_chain(&[clause]) {
                        after.push(form);
                    }
                }
                "finally_clause" => {
                    if let Some(inner) = code_children(clause).first().copied()
                        && let Some(form) = self.statement(inner)
                    {
                        after.push(form);
                    }
                }
                _ => {}
            }
        }
        let form = if arms.is_empty() {
            body
        } else {
            Form::select(body, arms)
        };
        if after.is_empty() {
            return form;
        }
        Form::Sequence(std::iter::once(form).chain(after).collect())
    }

    fn except_arm(&mut self, clause: Node<'_>) -> Arm {
        let (name, binding) = match clause.child_by_field_name("value") {
            Some(value) => match self.as_pattern(value) {
                Some((subject, alias)) => {
                    let name = self.text(subject).to_owned();
                    (name, self.bind_pattern(alias))
                }
                None => (self.text(value).to_owned(), Pattern::Ignored),
            },
            // A bare `except:` catches everything a program can raise, which
            // is a wider claim than `except Exception:` and is spelled as one.
            None => ("BaseException".to_owned(), Pattern::Ignored),
        };
        let body = code_children(clause)
            .into_iter()
            .find(|child| child.kind() == "block")
            .and_then(|block| self.statement(block))
            .unwrap_or(Form::Literal);
        Arm {
            pattern: Pattern::Variant {
                name,
                parts: vec![binding],
            },
            body,
        }
    }

    fn match_statement(&mut self, node: Node<'_>) -> Form {
        let scrutinee = node
            .child_by_field_name("subject")
            .map_or(Form::Literal, |subject| self.expression(subject));
        let mut arms = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            for case in named_children(body) {
                if case.kind() != "case_clause" {
                    continue;
                }
                let pattern = code_children(case)
                    .into_iter()
                    .find(|child| child.kind() == "case_pattern")
                    .map_or(Pattern::Ignored, |child| self.case_pattern(child));
                let body = case
                    .child_by_field_name("consequence")
                    .and_then(|block| self.statement(block))
                    .unwrap_or(Form::Literal);
                arms.push(Arm { pattern, body });
            }
        }
        Form::select(scrutinee, arms)
    }

    fn case_pattern(&mut self, node: Node<'_>) -> Pattern {
        let inner = if node.kind() == "case_pattern" {
            match code_children(node).first().copied() {
                // `case _:` has no child at all, and matches anything.
                None => return Pattern::Ignored,
                Some(inner) => inner,
            }
        } else {
            node
        };
        match inner.kind() {
            "dotted_name" | "identifier" => {
                let name = self.text(inner);
                // A bare lowercase name in a pattern CAPTURES; a dotted or
                // capitalised one names a value to compare against. Reading a
                // capture as a name would make every `case x:` a distinct arm
                // from every `case y:` while both match everything.
                if name.contains('.') || name.starts_with(char::is_uppercase) {
                    Pattern::Variant {
                        name: name.to_owned(),
                        parts: Vec::new(),
                    }
                } else {
                    self.bind_pattern(inner)
                }
            }
            "list_pattern" | "tuple_pattern" => Pattern::Tuple(
                code_children(inner)
                    .into_iter()
                    .map(|child| self.case_pattern(child))
                    .collect(),
            ),
            "class_pattern" => {
                let mut parts = Vec::new();
                let mut name = String::new();
                for (position, child) in code_children(inner).into_iter().enumerate() {
                    if position == 0 && child.kind() == "dotted_name" {
                        name = self.text(child).to_owned();
                        continue;
                    }
                    parts.push(self.case_pattern(child));
                }
                Pattern::Variant { name, parts }
            }
            "dict_pattern" => Pattern::Tuple(
                code_children(inner)
                    .into_iter()
                    .filter(|child| child.kind() == "case_pattern")
                    .map(|child| self.case_pattern(child))
                    .collect(),
            ),
            "keyword_pattern" => match code_children(inner).as_slice() {
                [_, value] => self.case_pattern(*value),
                _ => Pattern::Ignored,
            },
            "union_pattern" => Pattern::Variant {
                name: "Union".to_owned(),
                parts: code_children(inner)
                    .into_iter()
                    .map(|child| self.case_pattern(child))
                    .collect(),
            },
            "splat_pattern" => Pattern::Ignored,
            // A literal pattern compares against a value rather than binding.
            _ => Pattern::Variant {
                name: self.text(inner).to_owned(),
                parts: Vec::new(),
            },
        }
    }

    // -- bindings -----------------------------------------------------------

    /// Bind whatever names a target introduces, as a pattern.
    fn bind_pattern(&mut self, node: Node<'_>) -> Pattern {
        match node.kind() {
            "identifier" | "dotted_name" if node.kind() == "identifier" => {
                // `_` is the name Python gives a value it is throwing away.
                if self.text(node) == "_" {
                    return Pattern::Ignored;
                }
                match self.roles.bind(self.text(node)) {
                    Form::Local(index) => Pattern::Binding(index),
                    _ => Pattern::Ignored,
                }
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" | "expression_list" | "tuple"
            | "list" => Pattern::Tuple(
                code_children(node)
                    .into_iter()
                    .map(|child| self.bind_pattern(child))
                    .collect(),
            ),
            "list_splat_pattern"
            | "dictionary_splat_pattern"
            | "as_pattern_target"
            | "dotted_name" => match code_children(node).first().copied() {
                Some(inner) => self.bind_pattern(inner),
                None => Pattern::Ignored,
            },
            // Assigning to an attribute or a subscript stores into something
            // that already exists; it introduces no name.
            _ => Pattern::Ignored,
        }
    }

    // -- expressions --------------------------------------------------------

    fn expression(&mut self, node: Node<'_>) -> Form {
        match node.kind() {
            "parenthesized_expression" => match code_children(node).first().copied() {
                Some(inner) => self.expression(inner),
                None => Form::Literal,
            },
            "identifier" => {
                let name = self.text(node);
                // A name in screaming case that nothing here binds is a named
                // constant, and which constant it is *is* behavior. The
                // TypeScript frontend paid for this once: resolving both to a
                // hole made `ITEM_KIND_KEY` and `ITEM_KIND_VALUE` one thing, and
                // with them `keys` and `values`. Python spells constants the
                // same way and inherits the same erasure.
                if is_screaming_case(name) && !self.roles.is_value(name) {
                    return self.path(name);
                }
                self.roles.resolve(name)
            }
            "integer" | "float" => Form::Number(self.text(node).to_owned()),
            "true" | "false" => Form::Constant(self.text(node).to_owned()),
            // `None` is the absence a caller has to handle, and that is exactly
            // what `Option::None` and `undefined` are elsewhere.
            "none" => Form::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            },
            "ellipsis" => Form::Literal,
            "string" | "concatenated_string" => Form::Constant(self.text(node).to_owned()),
            // An EMPTY literal is construction and its elements are what it
            // does not have. A literal with elements carries them, and holding
            // it as a bare `Construct` would make `(a, b)` and `(b, a)` one
            // value — which is exactly the erasure that made `keys` and
            // `values` the same behavior in the Rust frontend.
            "list" => self.literal(node, "list"),
            "tuple" => self.literal(node, "tuple"),
            "set" => self.literal(node, "set"),
            "dictionary" => self.literal(node, "dict"),
            "pair" => Form::Variant {
                name: "pair".to_owned(),
                payload: vec![
                    node.child_by_field_name("key")
                        .map_or(Form::Literal, |key| self.expression(key)),
                    node.child_by_field_name("value")
                        .map_or(Form::Literal, |value| self.expression(value)),
                ],
            },
            "list_comprehension" => self.comprehension(node, Some("list")),
            "set_comprehension" => self.comprehension(node, Some("set")),
            "dictionary_comprehension" => self.comprehension(node, Some("dict")),
            // A generator produces the sequence without gathering it, which is
            // the whole difference between it and a list comprehension.
            "generator_expression" => self.comprehension(node, None),
            "unary_operator" | "not_operator" => self.unary(node),
            "binary_operator" | "boolean_operator" => self.binary(node),
            "comparison_operator" => self.comparison(node),
            "conditional_expression" => self.conditional(node),
            "call" => self.call(node),
            "attribute" => self.attribute(node),
            "subscript" => self.subscript(node),
            "lambda" => self.lambda(node),
            "assignment" | "augmented_assignment" => self.assignment(node),
            "named_expression" => self.walrus(node),
            "await" | "list_splat" | "dictionary_splat" | "type_conversion" => {
                match code_children(node).first().copied() {
                    Some(inner) => self.expression(inner),
                    None => Form::Literal,
                }
            }
            "yield" => Form::Variant {
                name: "Yield".to_owned(),
                payload: code_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            },
            "keyword_argument" => match node.child_by_field_name("value") {
                Some(value) => self.expression(value),
                None => Form::Literal,
            },
            // `X as y` reached as an expression rather than through a `with` or
            // an `except`: still a name given to a value.
            "as_pattern" => match self.as_pattern(node) {
                Some((subject, alias)) => {
                    let value = self.expression(subject);
                    let pattern = self.bind_pattern(alias);
                    Form::Let {
                        pattern: Box::new(pattern),
                        value: Box::new(value),
                    }
                }
                None => Form::Literal,
            },
            "expression_list" => Form::Sequence(
                code_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            ),
            _ => Form::Opaque {
                kind: node.kind().to_owned(),
                parts: code_children(node)
                    .into_iter()
                    .map(|child| self.expression(child))
                    .collect(),
            },
        }
    }

    fn unary(&mut self, node: Node<'_>) -> Form {
        let Some(operand) = node.child_by_field_name("argument") else {
            return Form::Literal;
        };
        let operand = self.expression(operand);
        let operator = if node.kind() == "not_operator" {
            "not"
        } else {
            node.child_by_field_name("operator")
                .map_or("", |operator| self.text(operator))
        };
        match operator {
            // Unary plus is the value it was given.
            "" | "+" => operand,
            _ => Form::Binary {
                operator: operator.to_owned(),
                left: Box::new(operand),
                right: Box::new(Form::Literal),
            },
        }
    }

    fn binary(&mut self, node: Node<'_>) -> Form {
        let operator = node
            .child_by_field_name("operator")
            .map_or(String::new(), |operator| self.text(operator).to_owned());
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

    /// `a < b < c` is `a < b and b < c`, and Python is the only language here
    /// that spells it in one node.
    ///
    /// Expanding it is what makes the chained form and the written-out form one
    /// thing. The operands are not evaluated twice at run time, and this form
    /// says nothing about how many times anything ran.
    fn comparison(&mut self, node: Node<'_>) -> Form {
        let mut cursor = node.walk();
        // Anonymous children are the operators and are what this reads
        // positionally, so a `\` continuation left among them would shift every
        // index by one and hand an operator token to `expression`. Measured: it
        // did, on 31 comparisons across CPython's standard library.
        let parts = node
            .children(&mut cursor)
            .filter(|child| !matches!(child.kind(), "comment" | "line_continuation"))
            .collect::<Vec<_>>();
        let mut comparisons: Vec<Form> = Vec::new();
        let mut index = 0;
        while index + 2 < parts.len() {
            let (left, right) = (parts[index], parts[index + 2]);
            // `not in` and `is not` are two tokens each, so the operator is
            // whatever text sits between the operands rather than one child.
            let operator = self
                .source
                .get(left.end_byte()..right.start_byte())
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if operator.is_empty() {
                break;
            }
            let left = self.expression(left);
            let right = self.expression(right);
            comparisons.push(Form::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            });
            index += 2;
        }
        let mut comparisons = comparisons.into_iter();
        let Some(first) = comparisons.next() else {
            return Form::Literal;
        };
        comparisons.fold(first, |left, right| Form::Binary {
            operator: "and".to_owned(),
            left: Box::new(left),
            right: Box::new(right),
        })
    }

    fn conditional(&mut self, node: Node<'_>) -> Form {
        // `x if c else y` writes the consequence first, and the grammar gives
        // the three parts no field names at all.
        let parts = code_children(node);
        let [consequence, condition, alternative] = parts.as_slice() else {
            return Form::Literal;
        };
        let condition = self.expression(*condition);
        let consequence = self.expression(*consequence);
        let alternative = self.expression(*alternative);
        Form::Branch {
            condition: Box::new(condition),
            consequence: Box::new(consequence),
            alternative: Some(Box::new(alternative)),
        }
    }

    /// A comprehension, in every spelling Python has for one.
    ///
    /// `[g(x) for x in xs if p(x)]` is a walk that keeps some elements and
    /// transforms them, which is exactly what the three-statement loop the
    /// cleanup pass rewrites also says. An `if` clause becomes a `Retain`
    /// around the sequence rather than a `Branch` in the body, because an
    /// element that fails the test is not produced at all.
    fn comprehension(&mut self, node: Node<'_>, container: Option<&str>) -> Form {
        let clauses = code_children(node);
        let Some(body) = clauses.first().copied() else {
            return Form::Literal;
        };
        // Every clause is bound before the body is read, because a later clause
        // can name what an earlier one bound and the body names all of them.
        let mut sequence = None;
        let mut item = Pattern::Ignored;
        let mut direction = Direction::Forward;
        let mut conditions = Vec::new();
        for clause in &clauses[1..] {
            match clause.kind() {
                "for_in_clause" => {
                    let Some(right) = clause.child_by_field_name("right") else {
                        continue;
                    };
                    let (walked, walked_direction) = self.sequence(right);
                    let bound = clause
                        .child_by_field_name("left")
                        .map_or(Pattern::Ignored, |left| self.bind_pattern(left));
                    match sequence.take() {
                        // A second `for` walks something each element of the
                        // first produced, which is a flattening.
                        Some(outer) => {
                            sequence = Some(Form::Sift {
                                sequence: Box::new(outer),
                                item: Box::new(std::mem::replace(&mut item, bound)),
                                body: Box::new(walked),
                            });
                        }
                        None => {
                            sequence = Some(walked);
                            item = bound;
                            direction = walked_direction;
                        }
                    }
                }
                "if_clause" => {
                    if let Some(condition) = code_children(*clause).first().copied() {
                        conditions.push(self.expression(condition));
                    }
                }
                _ => {}
            }
        }
        let Some(mut walked) = sequence else {
            return Form::Literal;
        };
        if direction == Direction::Backward {
            // `Traverse` carries a direction and a bare sequence does not, so
            // the reversal is held where it cannot be mistaken for a hole.
            walked = Form::Opaque {
                kind: "reversed".to_owned(),
                parts: vec![walked],
            };
        }
        let mut conditions = conditions.into_iter();
        if let Some(first) = conditions.next() {
            let condition = conditions.fold(first, |left, right| Form::Binary {
                operator: "and".to_owned(),
                left: Box::new(left),
                right: Box::new(right),
            });
            walked = Form::Retain {
                sequence: Box::new(walked),
                item: Box::new(item.clone()),
                body: Box::new(condition),
            };
        }
        let produced = self.expression(body);
        // `[x for x in xs]` transforms nothing, and saying it does would make a
        // copy look like work.
        let sequence = if pattern_is(&item, &produced) {
            walked
        } else {
            Form::Transform {
                sequence: Box::new(walked),
                item: Box::new(item),
                body: Box::new(produced),
            }
        };
        match container {
            Some(container) => Form::Collect {
                sequence: Box::new(sequence),
                container: Some(container.to_owned()),
            },
            None => sequence,
        }
    }

    fn call(&mut self, node: Node<'_>) -> Form {
        let Some(callee) = node.child_by_field_name("function") else {
            return Form::Literal;
        };
        let arguments = node
            .child_by_field_name("arguments")
            .map(|arguments| match arguments.kind() {
                // `sum(x for x in xs)` writes the generator where the argument
                // list would be.
                "generator_expression" => vec![arguments],
                _ => code_children(arguments),
            })
            .unwrap_or_default();

        // A method call keeps its receiver, because which value a method was
        // called on is most of what the call says.
        if callee.kind() == "attribute"
            && let Some(object) = callee.child_by_field_name("object")
            && let Some(name) = callee.child_by_field_name("attribute")
        {
            let receiver = self.expression(object);
            return Form::Method {
                name: self.text(name).to_owned(),
                receiver: Box::new(receiver),
                arguments: arguments
                    .iter()
                    .map(|argument| self.expression(*argument))
                    .collect(),
            };
        }

        // `list(xs)`, `set(xs)`, `dict(pairs)` gather a sequence into a named
        // container, which is what `.collect()` and `Array.from` are elsewhere.
        if callee.kind() == "identifier"
            && CONTAINERS.contains(&self.text(callee))
            && !self.roles.is_value(self.text(callee))
            && let [only] = arguments.as_slice()
        {
            let container = self.text(callee).to_owned();
            let sequence = self.expression(*only);
            return Form::Collect {
                sequence: Box::new(sequence),
                container: Some(container),
            };
        }

        // Calling a function defined elsewhere is not the same as calling one
        // the caller supplied. `helper(x)` names something a reader can go and
        // look at, and two delegations to different helpers are two behaviors.
        //
        // `infact-ts-normalize` does this at the same point, and Python needs it
        // more than TypeScript does: a class is constructed by naming it, so
        // `_UnixReadPipeTransport(self, pipe, protocol, waiter, extra)` and
        // `_ProactorReadPipeTransport(...)` differ in nothing but the name, and
        // reduced to the same six-hole form until this existed. Measured over
        // the installed corpus, 94.9% of calls had a hole for a callee.
        //
        // The name is kept bare rather than qualified by module. Two packages
        // that both define `Regex` do collide under this, which is the same
        // trade the TypeScript frontend made; qualifying would need to resolve
        // imports across files, and that is a different observation.
        let callee = match callee.kind() {
            "identifier" if !self.roles.is_value(self.text(callee)) => self.path(self.text(callee)),
            _ => self.expression(callee),
        };
        Form::Call {
            callee: Box::new(callee),
            arguments: arguments
                .iter()
                .map(|argument| self.expression(*argument))
                .collect(),
        }
    }

    /// A collection literal: construction when it is empty, the values it holds
    /// when it is not.
    ///
    /// `fuse_container_fills` recognizes the first shape, so keeping empty
    /// literals as bare `Construct` is what lets an append loop be seen at all.
    fn literal(&mut self, node: Node<'_>, container: &str) -> Form {
        let elements = code_children(node);
        if elements.is_empty() {
            return Form::Construct(container.to_owned());
        }
        Form::Variant {
            name: container.to_owned(),
            payload: elements
                .into_iter()
                .map(|element| self.expression(element))
                .collect(),
        }
    }

    fn attribute(&mut self, node: Node<'_>) -> Form {
        let Some(object) = node.child_by_field_name("object") else {
            return Form::Literal;
        };
        let Some(name) = node.child_by_field_name("attribute") else {
            return Form::Literal;
        };
        let value = self.expression(object);
        Form::Field {
            value: Box::new(value),
            name: self.text(name).to_owned(),
        }
    }

    /// `xs[i]` takes an element and `xs[a:b]` takes a run of them.
    ///
    /// Holding the second as one opaque node accounted for 72% of everything
    /// this crate could not read across CPython's standard library and the
    /// installed site-packages — 11,243 of 15,620 opaque nodes — which is what
    /// says it was worth a rule rather than a note.
    fn subscript(&mut self, node: Node<'_>) -> Form {
        let Some(value) = node.child_by_field_name("value") else {
            return Form::Literal;
        };
        let receiver = self.expression(value);
        let Some(index) = node.child_by_field_name("subscript") else {
            return Form::Method {
                name: "index".to_owned(),
                receiver: Box::new(receiver),
                arguments: vec![Form::Literal],
            };
        };
        if index.kind() != "slice" {
            let index = self.expression(index);
            return Form::Method {
                name: "index".to_owned(),
                receiver: Box::new(receiver),
                arguments: vec![index],
            };
        }
        // A bound the source left out is an end of the sequence, and it has to
        // be held rather than dropped: `xs[1:]` and `xs[:1]` are opposite
        // halves, and a form that kept only the `1` would make them one thing.
        let mut cursor = index.walk();
        let mut bounds = Vec::new();
        let mut pending = true;
        for part in index.children(&mut cursor) {
            if part.kind() == ":" {
                if pending {
                    bounds.push(Form::Variant {
                        name: "None".to_owned(),
                        payload: Vec::new(),
                    });
                }
                pending = true;
                continue;
            }
            if matches!(part.kind(), "comment" | "line_continuation") {
                continue;
            }
            bounds.push(self.expression(part));
            pending = false;
        }
        if pending {
            bounds.push(Form::Variant {
                name: "None".to_owned(),
                payload: Vec::new(),
            });
        }
        Form::Method {
            name: "slice".to_owned(),
            receiver: Box::new(receiver),
            arguments: bounds,
        }
    }

    fn lambda(&mut self, node: Node<'_>) -> Form {
        let parameters = node
            .child_by_field_name("parameters")
            .map(|parameters| self.bind_parameters(parameters))
            .unwrap_or_default();
        let body = match node.child_by_field_name("body") {
            Some(body) if body.kind() == "block" => valued(self.block(body)),
            Some(body) => self.expression(body),
            None => Form::Sequence(Vec::new()),
        };
        Form::Lambda {
            parameters,
            body: Box::new(body),
        }
    }

    /// Bind a parameter list, in either spelling, and report the patterns.
    fn bind_parameters(&mut self, parameters: Node<'_>) -> Vec<Pattern> {
        code_children(parameters)
            .into_iter()
            .filter_map(|parameter| match parameter.kind() {
                // `/` and `*` say how a caller may pass arguments, not what
                // the function does with them.
                "positional_separator" | "keyword_separator" => None,
                "identifier" => Some(self.bind_pattern(parameter)),
                "default_parameter" | "typed_default_parameter" | "typed_parameter" => parameter
                    .child_by_field_name("name")
                    .or_else(|| code_children(parameter).first().copied())
                    .map(|name| self.bind_pattern(name)),
                "list_splat_pattern" | "dictionary_splat_pattern" | "tuple_pattern" => {
                    Some(self.bind_pattern(parameter))
                }
                _ => None,
            })
            .collect()
    }

    fn assignment(&mut self, node: Node<'_>) -> Form {
        let Some(target) = node.child_by_field_name("left") else {
            return Form::Literal;
        };
        let Some(value) = node.child_by_field_name("right") else {
            // `x: int` with no value is an annotation and binds nothing.
            return Form::Literal;
        };
        let value = self.expression(value);
        if node.kind() == "augmented_assignment" {
            let operator = node
                .child_by_field_name("operator")
                .map_or(String::new(), |operator| self.text(operator).to_owned());
            let target = self.expression(target);
            return Form::Assign {
                operator,
                target: Box::new(target),
                value: Box::new(value),
            };
        }
        // Storing into an attribute or a subscript writes to something that
        // already exists; binding a name introduces one, and downstream the
        // two are different things.
        if !matches!(
            target.kind(),
            "identifier" | "pattern_list" | "tuple_pattern" | "list_pattern"
        ) {
            let target = self.expression(target);
            return Form::Assign {
                operator: "=".to_owned(),
                target: Box::new(target),
                value: Box::new(value),
            };
        }
        let pattern = self.bind_pattern(target);
        Form::Let {
            pattern: Box::new(pattern),
            value: Box::new(value),
        }
    }

    /// `(n := f())` binds and yields the value in one breath.
    fn walrus(&mut self, node: Node<'_>) -> Form {
        let value = node
            .child_by_field_name("value")
            .map_or(Form::Literal, |value| self.expression(value));
        let Some(name) = node.child_by_field_name("name") else {
            return value;
        };
        let pattern = self.bind_pattern(name);
        Form::Let {
            pattern: Box::new(pattern),
            value: Box::new(value),
        }
    }
}

/// Whether a pattern binds exactly the local a form names.
fn pattern_is(pattern: &Pattern, value: &Form) -> bool {
    matches!((pattern, value), (Pattern::Binding(bound), Form::Local(named)) if bound == named)
}

/// Declare what a function's caller supplies, so it reads as data.
///
/// `self` is declared with the rest: it is the receiver, exactly as `this` is
/// in TypeScript, and it is data the caller supplied rather than a name for
/// something defined elsewhere.
fn declare_parameters(function: Node<'_>, source: &[u8], normalizer: &mut Normalizer<'_>) {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return;
    };
    for parameter in code_children(parameters) {
        let name = match parameter.kind() {
            "identifier" => Some(parameter),
            "default_parameter" | "typed_default_parameter" | "typed_parameter" => parameter
                .child_by_field_name("name")
                .or_else(|| code_children(parameter).first().copied()),
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                code_children(parameter).first().copied()
            }
            _ => None,
        };
        if let Some(name) = name.filter(|name| name.kind() == "identifier") {
            normalizer.roles.declare(text(name, source));
        }
    }
}

pub fn normalize_function(function: Node<'_>, source: &[u8]) -> Form {
    let imports = imports_of(root_of(function), source);
    let mut normalizer = Normalizer::new(source, &imports);
    declare_parameters(function, source, &mut normalizer);
    match function.child_by_field_name("body") {
        Some(body) if body.kind() == "block" => valued(normalizer.block(body)),
        Some(body) => valued(normalizer.expression(body)),
        None => Form::Sequence(Vec::new()),
    }
}

/// Normalize one function, keeping where each step of the body was written.
pub fn normalize_function_located(
    function: Node<'_>,
    source: &[u8],
) -> (Form, Vec<StatementSpan>, Vec<LocatedForm>) {
    {
        let imports = imports_of(root_of(function), source);
        located_with(function, source, &imports)
    }
}

/// The body of `normalize_function_located`, with the file's imports supplied.
///
/// `normalize_file` reads a file's imports once rather than once per function.
fn located_with<'a>(
    function: Node<'_>,
    source: &'a [u8],
    imports: &'a Imports,
) -> (Form, Vec<StatementSpan>, Vec<LocatedForm>) {
    let mut normalizer = Normalizer::new(source, imports);
    declare_parameters(function, source, &mut normalizer);
    let (form, spans) = match function.child_by_field_name("body") {
        Some(body) if body.kind() == "block" => {
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
    let imports = imports_of(root_of(body), source);
    let mut normalizer = Normalizer::new(source, &imports);
    normalizer.block(body)
}

/// Normalize a file's top-level statements as one body.
///
/// A Python module is code that runs. Configuration, registration, and whole
/// scripts sit at the top level of a file, and a normalizer that only looked
/// inside functions would report nothing about them — silently, which is the
/// one failure mode worth going out of the way to avoid.
pub fn normalize_module(file: &ParsedFile) -> Form {
    let root = file.tree.root_node();
    let imports = imports_of(root, &file.source);
    let mut normalizer = Normalizer::new(&file.source, &imports);
    valued(normalizer.block(root))
}

/// Every named function, at every depth.
///
/// A method is a `function_definition` inside a class body and a nested helper
/// is one inside another function, so there is a single shape to find. Unlike
/// TypeScript, Python has no second way to give a function a name: `f = lambda:
/// ..` exists and PEP 8 tells you not to write it.
fn collect_functions<'a>(node: Node<'a>, output: &mut Vec<Node<'a>>) {
    if node.kind() == "function_definition" {
        output.push(node);
    }
    for child in named_children(node) {
        collect_functions(child, output);
    }
}

/// Normalize every function in a parsed Python file.
pub fn normalize_file(file: &ParsedFile) -> Vec<NormalizedFunction> {
    let mut nodes = Vec::new();
    collect_functions(file.tree.root_node(), &mut nodes);
    let imports = imports_of(file.tree.root_node(), &file.source);
    nodes
        .into_iter()
        .filter_map(|node| {
            let name = node.child_by_field_name("name")?;
            node.child_by_field_name("body")?;
            let located = located_with(node, &file.source, &imports);
            // A decorated function is written `@d` then `def f(): ..`, and the
            // decorators are part of what a reader would be pointed at.
            let outer = node
                .parent()
                .filter(|parent| parent.kind() == "decorated_definition")
                .unwrap_or(node);
            Some(NormalizedFunction {
                name: text(name, &file.source).to_owned(),
                start_byte: outer.start_byte() as u64,
                end_byte: outer.end_byte() as u64,
                start_line: u32::try_from(outer.start_position().row + 1).unwrap_or(u32::MAX),
                end_line: u32::try_from(outer.end_position().row + 1).unwrap_or(u32::MAX),
                form: located.0,
                statements: located.1,
                located: located.2,
                damaged: node.has_error(),
            })
        })
        .collect()
}
