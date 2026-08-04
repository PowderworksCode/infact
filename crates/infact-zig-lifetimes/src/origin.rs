//! Which Zig expressions are an origin, and what kind.
//!
//! Separated from the graph for the same reason `infact-rust-effects` keeps its
//! allocation table apart from its propagation: this is library knowledge about
//! Bun and the Zig standard library rather than a way of propagating anything,
//! and what belongs in it is a judgement that wants reviewing on its own.
//!
//! An origin is where a value comes from when it does not come from anywhere
//! else in the graph. Everything here is decided on the *written* callee, so it
//! is wrong wherever a name is reused for something else — which is why each
//! table is narrow, and why a name that could mean two things is left out
//! rather than guessed at.

/// Where a value came from, when the expression says so.
///
/// These are not ownership classes. `Allocation` does not mean the field owns
/// the memory — that needs a free site as well — it means the value was made
/// here rather than handed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// Memory came from an allocator at this expression.
    Allocation,
    /// The value went into a store that frees in bulk.
    Arena,
    /// The value is a process-lifetime singleton.
    Singleton,
    /// The value is a parameter of the enclosing function.
    Parameter,
    /// The value is the enclosing method's own receiver.
    Receiver,
    /// The value is `null`, `undefined` or a literal: it points at nothing yet.
    Empty,
}

impl Origin {
    /// The byte `infact-flow` propagates. The crate never interprets it; this
    /// is the only place the encoding is defined.
    pub fn label(self) -> u8 {
        match self {
            Origin::Allocation => 0,
            Origin::Arena => 1,
            Origin::Singleton => 2,
            Origin::Parameter => 3,
            Origin::Receiver => 4,
            Origin::Empty => 5,
        }
    }

    pub fn of_label(label: u8) -> Option<Self> {
        Some(match label {
            0 => Origin::Allocation,
            1 => Origin::Arena,
            2 => Origin::Singleton,
            3 => Origin::Parameter,
            4 => Origin::Receiver,
            5 => Origin::Empty,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Origin::Allocation => "allocation",
            Origin::Arena => "arena",
            Origin::Singleton => "singleton",
            Origin::Parameter => "parameter",
            Origin::Receiver => "receiver",
            Origin::Empty => "empty",
        }
    }
}

/// Every origin, so a consumer can enumerate them without knowing the encoding.
pub const ORIGINS: &[Origin] = &[
    Origin::Allocation,
    Origin::Arena,
    Origin::Singleton,
    Origin::Parameter,
    Origin::Receiver,
    Origin::Empty,
];

/// Calls that hand back memory the caller is now responsible for.
///
/// Matched on the trailing method name, because the allocator is spelled a
/// dozen ways — `alloc`, `allocator`, `bun.default_allocator`,
/// `this.arena.allocator()` — and the operation is the part that is stable.
///
/// `init` is deliberately absent. Bun writes both `Foo.init(alloc)` that
/// allocates and `Foo.init()` that fills a struct in place, and the name does
/// not say which. Resolving the callee and asking whether *it* allocates is the
/// answer, and that is a graph question rather than a table one.
const ALLOCATING_METHODS: &[&str] = &[
    "create",
    "alloc",
    "allocSentinel",
    "allocAdvanced",
    "dupe",
    "dupeZ",
    "realloc",
    "reallocate",
];

/// Free functions in Bun's prelude that allocate.
///
/// `bun.new(T, value)` is the house style for a heap-allocated `T`, and it is a
/// plain call rather than a method on an allocator.
const ALLOCATING_CALLS: &[&str] = &[
    "bun.new",
    "bun.create",
    "bun.newWithAlloc",
    "bun.handleOom",
    "bun.default_allocator.create",
];

/// Stores that own what is put in them and free it in bulk.
const ARENA_CALLS: &[&str] = &["Store.append", "store.append", ".arena.allocator"];

/// Getters that hand back something with the lifetime of the process.
const SINGLETON_CALLS: &[&str] = &[
    "bunVM",
    "getVM",
    "initGlobal",
    "VirtualMachine.get",
    "VirtualMachine.getMainThreadVM",
];

/// Values that point at nothing, and so say nothing about ownership.
const EMPTY_VALUES: &[&str] = &["null", "undefined", "0", ".{}", "&.{}"];

/// The origin an expression is, when its text settles it.
///
/// Returns the origin and the token that decided it, which becomes the
/// witness's terminating step — a reader checking the claim needs to see what
/// was matched, not just that something was.
pub fn of_expression(value: &str) -> Option<(Origin, String)> {
    let text = value.trim();
    if EMPTY_VALUES.contains(&text) {
        return Some((Origin::Empty, text.to_owned()));
    }
    // `try`, `await` and `&` wrap the expression without changing where the
    // value came from.
    let bare = text
        .trim_start_matches("try ")
        .trim_start_matches("await ")
        .trim_start_matches('&')
        .trim();

    for call in ARENA_CALLS {
        if bare.contains(call) {
            return Some((Origin::Arena, (*call).to_owned()));
        }
    }
    for call in ALLOCATING_CALLS {
        if bare.starts_with(call) && bare[call.len()..].starts_with('(') {
            return Some((Origin::Allocation, (*call).to_owned()));
        }
    }
    for call in SINGLETON_CALLS {
        if bare.contains(&format!("{call}(")) {
            return Some((Origin::Singleton, (*call).to_owned()));
        }
    }
    // A method call on anything: `alloc.create(Foo)`, `this.arena.dupe(u8, s)`.
    let (callee, _) = bare.split_once('(')?;
    let method = callee.rsplit('.').next()?;
    ALLOCATING_METHODS
        .contains(&method)
        .then(|| (Origin::Allocation, format!("{method}()")))
}

/// Whether an expression is a call at all, as opposed to a name or a literal.
pub fn is_call(value: &str) -> bool {
    value.contains('(') && value.trim_end().ends_with(')')
}

/// The callee of a call expression, as written: `alloc.create` from
/// `try alloc.create(Foo)`.
pub fn callee_of(value: &str) -> Option<&str> {
    let bare = value
        .trim()
        .trim_start_matches("try ")
        .trim_start_matches("await ")
        .trim_start_matches('&')
        .trim();
    let (callee, _) = bare.split_once('(')?;
    (!callee.is_empty()).then_some(callee)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_allocator_call_is_an_allocation_however_the_allocator_is_spelled() {
        for value in [
            "alloc.create(Foo)",
            "try allocator.create(Foo)",
            "try bun.default_allocator.create(Foo)",
            "this.arena.dupe(u8, name)",
        ] {
            assert_eq!(
                of_expression(value).map(|(origin, _)| origin),
                Some(Origin::Allocation),
                "{value}"
            );
        }
        assert_eq!(
            of_expression("bun.new(Foo, .{})").map(|(origin, _)| origin),
            Some(Origin::Allocation)
        );
    }

    /// `init` allocates in some containers and fills in place in others, and
    /// the name cannot say which. Resolving the callee answers it; a table
    /// cannot.
    #[test]
    fn init_is_not_treated_as_an_allocation() {
        assert_eq!(of_expression("Foo.init(alloc)"), None);
        assert_eq!(of_expression("try Watcher.init(dev)"), None);
    }

    #[test]
    fn a_bare_name_is_no_origin_at_all() {
        assert_eq!(of_expression("dev"), None);
        assert_eq!(of_expression("self.owner"), None);
    }

    #[test]
    fn nothing_valued_expressions_are_recognized() {
        for value in ["null", "undefined", ".{}"] {
            assert_eq!(
                of_expression(value).map(|(origin, _)| origin),
                Some(Origin::Empty),
                "{value}"
            );
        }
    }

    #[test]
    fn the_witness_token_says_what_was_matched() {
        assert_eq!(
            of_expression("try alloc.create(Foo)").map(|(_, token)| token),
            Some("create()".to_owned())
        );
        assert_eq!(
            of_expression("Data.Store.append(x)").map(|(_, token)| token),
            Some("Store.append".to_owned())
        );
    }

    #[test]
    fn every_origin_round_trips_through_its_label() {
        for origin in ORIGINS {
            assert_eq!(Origin::of_label(origin.label()), Some(*origin));
        }
    }

    #[test]
    fn a_callee_survives_the_wrappers_around_it() {
        assert_eq!(callee_of("try alloc.create(Foo)"), Some("alloc.create"));
        assert_eq!(callee_of("Watcher.init(dev)"), Some("Watcher.init"));
        assert_eq!(callee_of("dev"), None);
    }
}
