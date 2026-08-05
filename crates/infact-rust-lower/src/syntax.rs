//! A Rust syntax tree that keeps what it was written from.
//!
//! This is the opposite design choice from [`infact_normalize::Form`], and the
//! two are meant to coexist. A `Form` discards everything that is not behavior
//! so that two implementations can be compared; nothing can be emitted from it,
//! which `notes/LOWERING.md` measures. This tree discards nothing, so anything
//! lifted into it can be printed back out.
//!
//! Every node that is not yet understood becomes [`Expr::Verbatim`], holding
//! the source text exactly. That makes the round trip correct from the first
//! day and turns coverage into something measurable rather than something to
//! be finished before anything works: the printed output is always the same
//! program, and the number that improves is how much of it is structure rather
//! than text.
//!
//! Types, generics and attributes are held as source text on purpose. They are
//! carried through transformations rather than rewritten by them, and spelling
//! them out would triple the size of this file for no reach.

/// A `{ .. }` body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Block {
    pub statements: Vec<Stmt>,
}

/// One step of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// `let p: T = e else { .. };`
    Let {
        attributes: Vec<String>,
        pattern: Pat,
        annotation: Option<String>,
        value: Option<Expr>,
        /// The `else` of a `let`-`else`, which diverges rather than binds.
        diverging: Option<Block>,
    },
    /// An expression as a step. The last statement of a block is the block's
    /// value exactly when it has no semicolon, so whether one was written is
    /// part of what the code means and is kept.
    Expr {
        attributes: Vec<String>,
        value: Expr,
        semicolon: bool,
    },
    /// An item declared inside a body: a nested `fn`, `struct`, `use`.
    ///
    /// Held as text. A nested function's body is lifted separately, because
    /// every function in a file is lifted on its own.
    Item(String),
    /// A comment, kept so that printing a body does not delete its
    /// explanation.
    ///
    /// Whether it followed code on the same line is part of what it means.
    /// `// straitjacket-allow:` is scoped to its line, so moving it down to
    /// one of its own silently changes which statement it applies to.
    Comment { text: String, trailing: bool },
}

/// What a `while let` or `if let` tests, or a plain condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    Plain(Expr),
    /// `let P = e`, and the chain of them a `let`-chain allows.
    Let {
        pattern: Pat,
        value: Expr,
    },
    /// `a && let P = b && c`
    Chain(Vec<Condition>),
}

/// One alternative of a `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    pub attributes: Vec<String>,
    pub pattern: Pat,
    /// `if` on an arm. Its presence makes the order of arms meaningful.
    pub guard: Option<Expr>,
    pub body: Expr,
    /// Whether a `,` followed. A block-bodied arm may omit it.
    pub comma: bool,
}

/// A field of a struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldInit {
    /// `name: value`
    Named { name: String, value: Expr },
    /// `name`, which is `name: name`. Which was written is kept, because
    /// printing the long form where the short one was used is a diff.
    Shorthand(String),
    /// `..base`
    Base(Expr),
}

/// How a closure takes its captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    ByReference,
    ByValue,
}

/// The delimiter a macro was called with, which it may depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    Parenthesis,
    Bracket,
    Brace,
}

impl Delimiter {
    #[must_use]
    pub const fn open(self) -> char {
        match self {
            Self::Parenthesis => '(',
            Self::Bracket => '[',
            Self::Brace => '{',
        }
    }

    #[must_use]
    pub const fn close(self) -> char {
        match self {
            Self::Parenthesis => ')',
            Self::Bracket => ']',
            Self::Brace => '}',
        }
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Source text held exactly as written, for syntax not yet structured.
    ///
    /// Never wrong, and always a measurement: every one of these is a node the
    /// lift could describe and does not.
    Verbatim(String),
    /// A name or a path used as a value, including `self` and any `::<..>`.
    Path(String),
    /// A literal, with its suffix and its escapes as written.
    Literal(String),
    /// `value.name`, or `tuple.0`.
    Field {
        value: Box<Expr>,
        name: String,
    },
    Call {
        function: Box<Expr>,
        arguments: Vec<Expr>,
    },
    /// Held apart from `Call` because a receiver is not an argument: a rewrite
    /// that reorders arguments must not reach it.
    MethodCall {
        receiver: Box<Expr>,
        name: String,
        /// `::<T>` on the method, if one was written.
        turbofish: Option<String>,
        arguments: Vec<Expr>,
    },
    Binary {
        left: Box<Expr>,
        operator: String,
        right: Box<Expr>,
    },
    Unary {
        operator: String,
        operand: Box<Expr>,
    },
    /// `&e` and `&mut e`.
    ///
    /// The Rust lift into `Form` erases this as noise, which is what makes a
    /// getter returning `&self.field` unlowerable. It is the same distinction
    /// `baozi/LIFETIMES.tsv` exists to record for Zig, and it is kept here.
    Reference {
        mutable: bool,
        value: Box<Expr>,
    },
    Assign {
        target: Box<Expr>,
        /// `=`, or a compound operator such as `+=`.
        operator: String,
        value: Box<Expr>,
    },
    Cast {
        value: Box<Expr>,
        annotation: String,
    },
    Try(Box<Expr>),
    Await(Box<Expr>),
    /// `(e)`, kept because removing it can change what an operator binds to.
    Parenthesized(Box<Expr>),
    Closure {
        capture: Capture,
        /// Whether `async` was written.
        asynchronous: bool,
        parameters: Vec<Pat>,
        /// A closure with a return type must have a block body.
        annotation: Option<String>,
        body: Box<Expr>,
    },
    If {
        condition: Box<Condition>,
        consequence: Block,
        /// Another `If`, or a `Block`.
        alternative: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    /// `while c { .. }`, including `while let`.
    While {
        label: Option<String>,
        condition: Box<Condition>,
        body: Block,
    },
    For {
        label: Option<String>,
        pattern: Pat,
        sequence: Box<Expr>,
        body: Block,
    },
    Loop {
        label: Option<String>,
        body: Block,
    },
    Block {
        label: Option<String>,
        /// `unsafe`, `async`, `const`, `move`, in the order written.
        modifiers: Vec<String>,
        body: Block,
    },
    Return(Option<Box<Expr>>),
    Break {
        label: Option<String>,
        value: Option<Box<Expr>>,
    },
    Continue {
        label: Option<String>,
    },
    /// `T { field: value, ..base }`
    Struct {
        path: String,
        fields: Vec<FieldInit>,
    },
    Tuple(Vec<Expr>),
    /// `[a, b]`, or `[value; count]`.
    Array {
        elements: Vec<Expr>,
        repeat: Option<Box<Expr>>,
    },
    Index {
        value: Box<Expr>,
        index: Box<Expr>,
    },
    /// `a..b`, `..=b`, `..`
    Range {
        start: Option<Box<Expr>>,
        operator: String,
        end: Option<Box<Expr>>,
    },
    /// A macro call, with its tokens held as text.
    ///
    /// Nothing can be said about the inside of a macro without expanding it,
    /// and the tokens are not necessarily an expression at all — `matches!`
    /// takes a pattern. Held verbatim, a macro round-trips exactly.
    Macro {
        path: String,
        delimiter: Delimiter,
        tokens: String,
    },
    /// `()`
    Unit,
}

/// A pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    /// Source text held exactly as written.
    Verbatim(String),
    /// A name a pattern introduces, with `ref` and `mut` as written.
    Binding {
        by_reference: bool,
        mutable: bool,
        name: String,
        /// `name @ subpattern`
        subpattern: Option<Box<Pat>>,
    },
    /// `_`
    Wild,
    /// `..`
    Rest,
    /// A path naming a unit variant or a constant.
    Path(String),
    Literal(String),
    /// `Some(x)`, `Ok(v)`
    TupleStruct {
        path: String,
        elements: Vec<Pat>,
    },
    /// `Point { x, y: other, .. }`
    Struct {
        path: String,
        fields: Vec<FieldPat>,
        rest: bool,
    },
    Tuple(Vec<Pat>),
    Slice(Vec<Pat>),
    /// `A | B`, which binds the same names in every alternative.
    ///
    /// The `Form` lift makes this `Pattern::Ignored`, which loses the bindings
    /// and turns them into free variables. Kept here.
    Or(Vec<Pat>),
    Reference {
        mutable: bool,
        pattern: Box<Pat>,
    },
    Range {
        start: Option<String>,
        operator: String,
        end: Option<String>,
    },
}

/// One field of a struct pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldPat {
    Named {
        name: String,
        pattern: Pat,
    },
    /// `name`, or `ref mut name`.
    Shorthand {
        by_reference: bool,
        mutable: bool,
        name: String,
    },
}

/// A function body lifted from a file, with where it came from.
#[derive(Debug, Clone)]
pub struct LiftedBody {
    pub name: String,
    /// Where the body's braces are, so what was lifted can be replaced by what
    /// is printed.
    pub start_byte: u64,
    pub end_byte: u64,
    pub start_line: u32,
    pub block: Block,
}

/// How much of a lift is structure and how much is still text.
///
/// The round trip is correct whatever this says, because verbatim text prints
/// back as itself. This is what says how much of the tree a rewrite could
/// actually reach.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    pub expressions: u64,
    pub verbatim_expressions: u64,
    pub patterns: u64,
    pub verbatim_patterns: u64,
}

impl Coverage {
    #[must_use]
    pub fn structured(&self) -> f64 {
        let total = self.expressions + self.patterns;
        if total == 0 {
            return 1.0;
        }
        let verbatim = self.verbatim_expressions + self.verbatim_patterns;
        (total - verbatim) as f64 / total as f64
    }

    pub fn add(&mut self, other: Self) {
        self.expressions += other.expressions;
        self.verbatim_expressions += other.verbatim_expressions;
        self.patterns += other.patterns;
        self.verbatim_patterns += other.verbatim_patterns;
    }
}

impl Block {
    /// Count what is structure and what is still held as text.
    #[must_use]
    pub fn coverage(&self) -> Coverage {
        let mut coverage = Coverage::default();
        self.measure(&mut coverage);
        coverage
    }

    fn measure(&self, coverage: &mut Coverage) {
        for statement in &self.statements {
            match statement {
                Stmt::Let {
                    pattern,
                    value,
                    diverging,
                    ..
                } => {
                    pattern.measure(coverage);
                    if let Some(value) = value {
                        value.measure(coverage);
                    }
                    if let Some(block) = diverging {
                        block.measure(coverage);
                    }
                }
                Stmt::Expr { value, .. } => value.measure(coverage),
                Stmt::Item(_) | Stmt::Comment { .. } => {}
            }
        }
    }
}

impl Condition {
    fn measure(&self, coverage: &mut Coverage) {
        match self {
            Self::Plain(value) => value.measure(coverage),
            Self::Let { pattern, value } => {
                pattern.measure(coverage);
                value.measure(coverage);
            }
            Self::Chain(parts) => {
                for part in parts {
                    part.measure(coverage);
                }
            }
        }
    }
}

impl Expr {
    fn measure(&self, coverage: &mut Coverage) {
        coverage.expressions += 1;
        if matches!(self, Self::Verbatim(_)) {
            coverage.verbatim_expressions += 1;
        }
        match self {
            Self::Verbatim(_)
            | Self::Path(_)
            | Self::Literal(_)
            | Self::Unit
            | Self::Continue { .. }
            | Self::Macro { .. } => {}
            Self::Field { value, .. }
            | Self::Unary { operand: value, .. }
            | Self::Reference { value, .. }
            | Self::Cast { value, .. }
            | Self::Try(value)
            | Self::Await(value)
            | Self::Parenthesized(value)
            | Self::Index { value, .. } => value.measure(coverage),
            Self::Call {
                function: head,
                arguments,
            }
            | Self::MethodCall {
                receiver: head,
                arguments,
                ..
            } => {
                head.measure(coverage);
                for argument in arguments {
                    argument.measure(coverage);
                }
            }
            Self::Binary { left, right, .. }
            | Self::Assign {
                target: left,
                value: right,
                ..
            } => {
                left.measure(coverage);
                right.measure(coverage);
            }
            Self::Closure {
                parameters, body, ..
            } => {
                for parameter in parameters {
                    parameter.measure(coverage);
                }
                body.measure(coverage);
            }
            Self::If {
                condition,
                consequence,
                alternative,
            } => {
                condition.measure(coverage);
                consequence.measure(coverage);
                if let Some(alternative) = alternative {
                    alternative.measure(coverage);
                }
            }
            Self::Match { scrutinee, arms } => {
                scrutinee.measure(coverage);
                for arm in arms {
                    arm.pattern.measure(coverage);
                    if let Some(guard) = &arm.guard {
                        guard.measure(coverage);
                    }
                    arm.body.measure(coverage);
                }
            }
            Self::While {
                condition, body, ..
            } => {
                condition.measure(coverage);
                body.measure(coverage);
            }
            Self::For {
                pattern,
                sequence,
                body,
                ..
            } => {
                pattern.measure(coverage);
                sequence.measure(coverage);
                body.measure(coverage);
            }
            Self::Loop { body, .. } | Self::Block { body, .. } => body.measure(coverage),
            Self::Return(value) | Self::Break { value, .. } => {
                if let Some(value) = value {
                    value.measure(coverage);
                }
            }
            Self::Struct { fields, .. } => {
                for field in fields {
                    match field {
                        FieldInit::Named { value, .. } | FieldInit::Base(value) => {
                            value.measure(coverage);
                        }
                        FieldInit::Shorthand(_) => {}
                    }
                }
            }
            Self::Tuple(parts) => {
                for part in parts {
                    part.measure(coverage);
                }
            }
            Self::Array { elements, repeat } => {
                for element in elements {
                    element.measure(coverage);
                }
                if let Some(repeat) = repeat {
                    repeat.measure(coverage);
                }
            }
            Self::Range { start, end, .. } => {
                if let Some(start) = start {
                    start.measure(coverage);
                }
                if let Some(end) = end {
                    end.measure(coverage);
                }
            }
        }
    }
}

impl Pat {
    fn measure(&self, coverage: &mut Coverage) {
        coverage.patterns += 1;
        if matches!(self, Self::Verbatim(_)) {
            coverage.verbatim_patterns += 1;
        }
        match self {
            Self::Verbatim(_)
            | Self::Wild
            | Self::Rest
            | Self::Path(_)
            | Self::Literal(_)
            | Self::Range { .. } => {}
            Self::Binding { subpattern, .. } => {
                if let Some(subpattern) = subpattern {
                    subpattern.measure(coverage);
                }
            }
            Self::TupleStruct {
                elements: parts, ..
            }
            | Self::Tuple(parts)
            | Self::Slice(parts)
            | Self::Or(parts) => {
                for part in parts {
                    part.measure(coverage);
                }
            }
            Self::Struct { fields, .. } => {
                for field in fields {
                    if let FieldPat::Named { pattern, .. } = field {
                        pattern.measure(coverage);
                    }
                }
            }
            Self::Reference { pattern, .. } => pattern.measure(coverage),
        }
    }
}
