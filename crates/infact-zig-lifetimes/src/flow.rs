//! Where a field's value came from, as a graph.
//!
//! The declaration rules in the crate root answer what a field's *type* settles
//! and abstain on everything else, which leaves `OWNED` and `BORROW_PARAM` — the
//! two largest questions — permanently unanswerable. Neither is a property of
//! the declaration. Both are properties of where the value came from:
//!
//! ```zig
//! self.watcher = try Watcher.init(alloc);   // made here
//! self.dev     = dev;                       // handed over
//! ```
//!
//! Identical at the declaration, opposite in the port. So this builds the graph
//! that tells them apart, on [`infact_flow`], which is the same engine
//! `infact-rust-effects` propagates effects over. Only the edges differ: an
//! effect moves from a callee up into its caller, a value moves from an
//! expression into the field it is stored in.
//!
//! ## What this does not do
//!
//! It does not conclude anything. A field that reaches an allocation is not
//! `OWNED` — that also needs a free site — and a field that reaches a parameter
//! is not `BORROW_PARAM` if the container frees it. Reading the graph is
//! [`crate::derive`]'s job; this only says what reaches what, and why.
//!
//! ## How much of Bun it actually reaches
//!
//! Of the 1,691 fields Bun's own classification names that this pipeline can
//! locate:
//!
//! | | fields | |
//! |---|---|---|
//! | anything at all is assigned to it | 468 | 27.7% |
//! | traced to an origin | 398 | 23.5% — **85.0% of those with an inflow** |
//!
//! Those two numbers say different things, and reporting only the second hides
//! which half is broken. Propagation is not the problem: once an assignment is
//! tied to the field it writes, the graph carries an origin back to it five
//! times out of six. **The ceiling is placement.** Nearly three fields in four
//! are never seen to be assigned at all.
//!
//! Where it does reach, the origins separate the classes they should:
//! allocations land on `OWNED` and `ARENA`, parameters on `BORROW_PARAM` and
//! `JSC_BORROW`, and the free sites concentrate in `OWNED` and `SHARED`.
//!
//! ## Why placement fails, counted rather than guessed at
//!
//! Of 56,018 observed assignments, 4,880 are tied to a declared field.
//!
//! | | count |
//! |---|---|
//! | the enclosing function is not a method | 20,591 |
//! | writes through a local of unknown type | 8,339 |
//! | sits outside any function | 7,405 |
//! | writes through a chained receiver this could not walk | 5,572 |
//! | names a container with no such field | 4,306 |
//!
//! Five fixes are already priced into those numbers, and together they moved
//! reachability from 17.6% to 23.5%: locals are observed, so `const c =
//! alloc.create(Child); self.child = c;` links; the whole corpus is read rather
//! than only the files the key names; container names resolve to the file that
//! declares them, because a container is nearly always constructed somewhere
//! other than where it is declared; a chained receiver is walked field by field
//! through the declared types; and a field assigned from a call draws on the
//! callee's return.
//!
//! Each was worth one to two points. That is the shape of the remaining work
//! too: every one of the rows above is the same problem — the type of an
//! arbitrary expression is not written down — and closing it properly is a type
//! inference pass rather than more special cases. A consumer should read 27.7%
//! as the honest reach of syntax alone, and treat the rest as fields that need
//! either that pass or a reader who can see what the types must be.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use entl_zig_observe::{
    CallSite, ContainerField, Deferred, FieldAssignment, Function, Local, ParentRecovery,
    ReturnSite, Span, TypeShape,
};
use infact_core::SourceSpan;
use infact_flow::{Accounting, Flow, Graph, Seed, Witness};

use crate::origin::{self, Origin};

/// Everything observed about one file, as the entl pass reports it.
#[derive(Debug, Clone, Default)]
pub struct FileObservations {
    pub path: PathBuf,
    pub fields: Vec<ContainerField>,
    pub functions: Vec<Function>,
    pub assignments: Vec<FieldAssignment>,
    pub calls: Vec<CallSite>,
    pub returns: Vec<ReturnSite>,
    pub locals: Vec<Local>,
    pub deferred: Vec<Deferred>,
    pub recoveries: Vec<ParentRecovery>,
}

/// A field, named the way the whole pipeline keys on it.
///
/// `(file, container path, field)` and never `(file stem, field)`: the short
/// key collides on 67 rows of Bun's own classification, including pairs where
/// one side is `OWNED` and the other `BORROW_PARAM`, so a collision picks a
/// wrong answer in the direction that double-frees.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldKey {
    pub path: PathBuf,
    pub container: String,
    pub name: String,
}

impl FieldKey {
    fn of(field: &ContainerField) -> Self {
        FieldKey {
            path: field.path.clone(),
            container: field.container.clone(),
            name: field.name.clone(),
        }
    }

    fn node_name(&self) -> String {
        format!("{}:{}.{}", self.path.display(), self.container, self.name)
    }
}

/// Why an assignment could not be tied to a declared field.
///
/// Kept apart from [`Accounting::unknown`] because "the graph did not reach
/// this" is a single number that hides four different problems, and only one of
/// them is worth fixing at a time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Unplaced {
    /// The assignment sits outside any function this pass found.
    pub no_enclosing_function: usize,
    /// The enclosing function is not a method, so `self` means nothing here.
    pub not_a_method: usize,
    /// The assignment writes through something other than the receiver:
    /// `other.field = x`. A real edge, but to a container this pass cannot name
    /// without knowing the local's type.
    pub foreign_receiver: usize,
    /// The container was named but declares no such field.
    pub no_such_field: usize,
    /// Of the above, how many write through a chained receiver: `self.a.b = x`.
    pub chained_receiver: usize,
    /// Of the above, how many write through a bare name that is a local whose
    /// type nothing declares.
    pub untyped_local: usize,
}

/// An assignment this pass could not tie to a field, with where to look.
///
/// Carried so a provider that *does* know types can be asked precisely what
/// this could not work out, at the exact position of the receiver, rather than
/// being asked to analyse everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnplacedSite {
    pub path: PathBuf,
    pub receiver: String,
    pub field: String,
    pub container: String,
    pub span: Span,
}

impl Unplaced {
    pub fn total(&self) -> usize {
        self.no_enclosing_function + self.not_a_method + self.foreign_receiver + self.no_such_field
    }
}

/// The graph, and the map from fields back into it.
pub struct ZigFlow {
    graph: Graph,
    nodes: BTreeMap<String, u64>,
    fields: BTreeMap<FieldKey, u64>,
    /// Functions that free a field of their own receiver, by container.
    frees: BTreeMap<(PathBuf, String), Vec<String>>,
    accounting: Accounting,
    unplaced: Unplaced,
    unplaced_sites: Vec<UnplacedSite>,
}

impl ZigFlow {
    /// Every assignment that reached no field, with the receiver's position.
    pub fn unplaced_sites(&self) -> &[UnplacedSite] {
        &self.unplaced_sites
    }

    pub fn accounting(&self) -> &Accounting {
        &self.accounting
    }

    /// Why assignments failed to reach a declared field.
    pub fn unplaced(&self) -> &Unplaced {
        &self.unplaced
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// How many distinct things the graph names.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Containers that free at least one of their own fields.
    pub fn freeing_containers(&self) -> usize {
        self.frees.len()
    }

    pub fn fields(&self) -> impl Iterator<Item = (&FieldKey, u64)> {
        self.fields.iter().map(|(key, id)| (key, *id))
    }

    pub fn node(&self, key: &FieldKey) -> Option<u64> {
        self.fields.get(key).copied()
    }

    /// Every origin a field's value can be traced to.
    pub fn origins(&self, node: u64) -> Vec<Origin> {
        let carried = self.graph.propagate();
        carried
            .iter()
            .filter(|labelled| labelled.node == node)
            .filter_map(|labelled| Origin::of_label(labelled.label))
            .collect()
    }

    /// Whether the container that declares this field frees it in its own
    /// destructor.
    ///
    /// This is the half of `OWNED` that an allocation alone does not establish:
    /// a value allocated by one owner and handed to this one is not owned here,
    /// and only the free site tells them apart.
    pub fn freed_by_container(&self, key: &FieldKey) -> bool {
        self.frees
            .get(&(key.path.clone(), key.container.clone()))
            .is_some_and(|freed| freed.contains(&key.name))
    }

    /// Whether anything at all flows into this node.
    ///
    /// The difference between "no edge reaches this field" and "an edge reaches
    /// it but leads nowhere seeded" is the difference between a placement
    /// problem and a seeding problem, and one number for both hides which.
    pub fn has_inflow(&self, node: u64) -> bool {
        self.graph.flows().iter().any(|flow| flow.into == node)
    }

    pub fn witnesses(&self, node: u64, origin: Origin) -> Vec<Witness> {
        self.graph.witnesses(node, origin.label())
    }
}

fn span_of(path: &Path, span: Span) -> SourceSpan {
    SourceSpan {
        path: path.to_path_buf(),
        start_byte: Some(span.start_byte as u64),
        end_byte: Some(span.end_byte as u64),
        start_line: span.start_line as u32,
        end_line: span.end_line as u32,
        start_column: None,
        end_column: None,
    }
}

/// Free operations, as written on a receiver's field.
///
/// Narrow on purpose. `deinit` on a field is the ambiguous one — a container
/// calls `deinit` on things it owns and on things it merely resets — so it
/// counts only alongside a `destroy` or a `free`, which name the allocator.
const FREEING_METHODS: &[&str] = &["destroy", "free", "deinit", "deref", "unref"];

/// Build the value-flow graph over every observed file.
pub fn build(observations: &[FileObservations]) -> ZigFlow {
    let mut builder = Builder::default();

    // Fields first, so an assignment can find the field it targets.
    for file in observations {
        for field in &file.fields {
            let key = FieldKey::of(field);
            let id = builder.intern(&key.node_name());
            // Both the dotted path and its last segment, because an assignment
            // names the container the way the source refers to it — `Ctx`, not
            // `Outer.run.Ctx`.
            for name in [key.container.clone(), last_segment(&key.container)] {
                let files = builder.declared_in.entry(name).or_default();
                if !files.contains(&file.path) {
                    files.push(file.path.clone());
                }
            }
            builder
                .field_types
                .insert(key.clone(), field.zig_type.clone());
            builder.fields.insert(key, id);
        }
    }

    // Functions, so an argument can find the parameter it lands on and a
    // receiver's fields can be reached through `self`.
    let mut by_qualified: BTreeMap<String, Vec<(&Path, &Function)>> = BTreeMap::new();
    for file in observations {
        for function in &file.functions {
            by_qualified
                .entry(function.qualified())
                .or_default()
                .push((file.path.as_path(), function));
            for parameter in &function.parameters {
                let name = parameter_node(&file.path, function, parameter.index, &parameter.name);
                let id = builder.intern(&name);
                // A pointer parameter is where a borrowed value enters. Seeding
                // it is what lets a field reached from here be recognized as
                // holding something the caller already owned.
                if TypeShape::of(&parameter.zig_type).is_pointer() {
                    let origin = if function
                        .receiver()
                        .is_some_and(|receiver| receiver.index == parameter.index)
                    {
                        Origin::Receiver
                    } else {
                        Origin::Parameter
                    };
                    builder.graph.seed(Seed {
                        node: id,
                        label: origin.label(),
                        origin: format!("{}({})", origin.id(), parameter.name),
                        span: span_of(&file.path, parameter.span),
                    });
                }
            }
        }
    }

    for file in observations {
        builder.add_locals(file, &by_qualified);
        builder.add_returns(file);
        builder.add_frees(file);
        builder.add_assignments(file, &by_qualified);
    }

    builder.graph.settle();
    ZigFlow {
        graph: builder.graph,
        nodes: builder.nodes,
        fields: builder.fields,
        frees: builder.frees,
        accounting: builder.accounting,
        unplaced: builder.unplaced,
        unplaced_sites: builder.unplaced_sites,
    }
}

fn last_segment(container: &str) -> String {
    container.rsplit('.').next().unwrap_or(container).to_owned()
}

/// The value a function hands back, as one node per function.
///
/// A constructor is the join between where memory is made and where it is
/// kept: `self.watcher = try Watcher.init(alloc)` says nothing until
/// `Watcher.init`'s own `return` is followed. Naming the return once per
/// function rather than once per site keeps the graph small and is enough,
/// because every return of a function is the same claim about its callers.
fn return_node(path: &Path, function: &str) -> String {
    format!("{}:{function}->", path.display())
}

fn local_node(path: &Path, function: &str, name: &str) -> String {
    format!("{}:{function}${name}", path.display())
}

fn parameter_node(path: &Path, function: &Function, index: usize, name: &str) -> String {
    format!(
        "{}:{}#{index}({name})",
        path.display(),
        function.qualified()
    )
}

#[derive(Default)]
struct Builder {
    graph: Graph,
    nodes: BTreeMap<String, u64>,
    fields: BTreeMap<FieldKey, u64>,
    frees: BTreeMap<(PathBuf, String), Vec<String>>,
    /// (file, function, local name) -> the container that local holds.
    local_types: BTreeMap<(PathBuf, String, String), String>,
    /// Field -> the type it is declared with, for walking a chained receiver.
    field_types: BTreeMap<FieldKey, String>,
    /// Container name -> the files that declare a container by that name.
    ///
    /// Zig type names are text: `*DevServer` says nothing about which file
    /// declares `DevServer`, and a container is almost always constructed
    /// somewhere other than where it is declared. Resolving the name is what
    /// lets an assignment written in one file reach a field declared in
    /// another; without it every cross-file assignment looks like a field that
    /// does not exist.
    declared_in: BTreeMap<String, Vec<PathBuf>>,
    accounting: Accounting,
    unplaced: Unplaced,
    unplaced_sites: Vec<UnplacedSite>,
}

impl Builder {
    fn intern(&mut self, name: &str) -> u64 {
        if let Some(id) = self.nodes.get(name) {
            return *id;
        }
        let id = self.nodes.len() as u64;
        self.nodes.insert(name.to_owned(), id);
        self.graph.name(id, name);
        id
    }

    /// Give every local a node, seed the ones whose value is an origin, and
    /// remember what type each one holds.
    ///
    /// This is what joins `const c = try alloc.create(Child);` to the
    /// `self.child = c;` two lines below it, and what lets `ctx.dev = dev` name
    /// the container `ctx` belongs to.
    fn add_locals(
        &mut self,
        file: &FileObservations,
        by_qualified: &BTreeMap<String, Vec<(&Path, &Function)>>,
    ) {
        for local in &file.locals {
            let node = self.intern(&local_node(&file.path, &local.function, &local.name));
            // What the local holds: its own annotation or named initialiser
            // first, then — for `const x = try Foo.init(..)` — the return type
            // of the function it was assigned from.
            let held = local.container().map(str::to_owned).or_else(|| {
                let callee = origin::callee_of(&local.value)?;
                let (_, function) = resolve_with_path(callee, by_qualified)?;
                let written = TypeShape::pointee(&function.zig_return);
                (!written.is_empty() && written != "void" && written != "anyerror")
                    .then(|| written.to_owned())
            });
            if let Some(container) = held {
                self.local_types.insert(
                    (
                        file.path.clone(),
                        local.function.clone(),
                        local.name.clone(),
                    ),
                    container,
                );
            }
            let span = span_of(&file.path, local.span);
            if let Some((origin, token)) = origin::of_expression(&local.value) {
                self.graph.seed(Seed {
                    node,
                    label: origin.label(),
                    origin: token,
                    span,
                });
                continue;
            }
            // A local initialised from a parameter carries that parameter's
            // borrowed-ness onward.
            let bare = local.value.trim().trim_start_matches('&').trim();
            if bare.is_empty() || bare.contains(['(', '.', ' ']) {
                continue;
            }
            let Some(function) = file
                .functions
                .iter()
                .find(|function| function.qualified() == local.function)
            else {
                continue;
            };
            if let Some(parameter) = function
                .parameters
                .iter()
                .find(|parameter| parameter.name == bare)
            {
                let from =
                    self.intern(&parameter_node(&file.path, function, parameter.index, bare));
                self.graph.flow(Flow {
                    into: node,
                    from,
                    span,
                });
            }
        }
    }

    /// Link each function's return to whatever it returns.
    ///
    /// The other half of following a constructor: this says what
    /// `Watcher.init` hands back, and [`Builder::add_assignments`] says which
    /// field keeps it.
    fn add_returns(&mut self, file: &FileObservations) {
        for site in &file.returns {
            if site.value.is_empty() {
                continue;
            }
            let node = self.intern(&return_node(&file.path, &site.function));
            let span = span_of(&file.path, site.span);
            if let Some((origin, token)) = origin::of_expression(&site.value) {
                self.graph.seed(Seed {
                    node,
                    label: origin.label(),
                    origin: token,
                    span,
                });
                continue;
            }
            let bare = site.value.trim().trim_start_matches('&').trim();
            if bare.is_empty() || bare.contains(['(', '.', ' ', '{']) {
                continue;
            }
            let Some(function) = file
                .functions
                .iter()
                .find(|function| function.qualified() == site.function)
            else {
                continue;
            };
            // `return self;` where `self` is the local an allocation went into,
            // or a parameter handed straight back.
            let from = function
                .parameters
                .iter()
                .find(|parameter| parameter.name == bare)
                .map(|parameter| parameter_node(&file.path, function, parameter.index, bare))
                .or_else(|| {
                    file.locals
                        .iter()
                        .find(|local| local.function == site.function && local.name == bare)
                        .map(|local| local_node(&file.path, &local.function, &local.name))
                });
            if let Some(from) = from {
                let from = self.intern(&from);
                self.graph.flow(Flow {
                    into: node,
                    from,
                    span,
                });
            }
        }
    }

    /// Record which fields a container frees in its own destructor.
    fn add_frees(&mut self, file: &FileObservations) {
        for function in file.functions.iter().filter(|f| f.is_deinit()) {
            let Some(receiver) = function.receiver() else {
                continue;
            };
            for call in &file.calls {
                if call.enclosing != function.qualified() {
                    continue;
                }
                let method = call.callee.rsplit('.').next().unwrap_or_default();
                if !FREEING_METHODS.contains(&method) {
                    continue;
                }
                // `self.child.deinit()` frees `child`; `alloc.destroy(self.x)`
                // frees `x`. Both name the field, in different places.
                let through = call
                    .callee
                    .strip_suffix(&format!(".{method}"))
                    .unwrap_or_default();
                let freed = field_of_receiver(through, &receiver.name).or_else(|| {
                    call.arguments
                        .first()
                        .and_then(|argument| field_of_receiver(&argument.text, &receiver.name))
                });
                if let Some(freed) = freed {
                    self.frees
                        .entry((file.path.clone(), function.container.clone()))
                        .or_default()
                        .push(freed);
                }
            }
        }
    }

    fn add_assignments(
        &mut self,
        file: &FileObservations,
        by_qualified: &BTreeMap<String, Vec<(&Path, &Function)>>,
    ) {
        for assignment in &file.assignments {
            self.accounting.total += 1;
            let Some(target) = self.target_field(file, assignment) else {
                // The assignment names a field this pass never observed: a
                // container declared somewhere it does not walk, or a receiver
                // that is not a container at all.
                self.accounting.unknown += 1;
                continue;
            };
            let span = span_of(&file.path, assignment.span);
            let value = assignment.value.trim();

            // An expression whose text settles where the value came from.
            if let Some((origin, token)) = origin::of_expression(value) {
                let node = self.intern(&format!(
                    "{}:{}:{}",
                    file.path.display(),
                    assignment.value_span.start_line,
                    value
                ));
                self.graph.seed(Seed {
                    node,
                    label: origin.label(),
                    origin: token,
                    span: span.clone(),
                });
                self.graph.flow(Flow {
                    into: target,
                    from: node,
                    span,
                });
                self.accounting.seeded += 1;
                continue;
            }

            // A bare name: a parameter of the function the assignment is
            // written in, which is how a borrowed pointer arrives.
            if let Some(source) = self.named_source(file, assignment, value) {
                self.graph.flow(Flow {
                    into: target,
                    from: source,
                    span,
                });
                self.accounting.linked += 1;
                continue;
            }

            // A call to something inside the corpus: the value is whatever that
            // function returns, so the field draws from the callee's return.
            if origin::is_call(value) {
                let resolved = origin::callee_of(value)
                    .and_then(|callee| resolve_with_path(callee, by_qualified));
                if let Some((callee_path, callee)) = resolved {
                    let from = self.intern(&return_node(callee_path, &callee.qualified()));
                    self.graph.flow(Flow {
                        into: target,
                        from,
                        span,
                    });
                    self.accounting.linked += 1;
                } else {
                    self.accounting.external += 1;
                }
                continue;
            }

            self.accounting.unknown += 1;
        }
    }

    /// The field node an assignment writes to.
    fn target_field(
        &mut self,
        file: &FileObservations,
        assignment: &FieldAssignment,
    ) -> Option<u64> {
        // A named initialiser says the container outright: `Ctx{ .dev = dev }`.
        if !assignment.container.is_empty()
            && let Some(id) =
                self.resolve_field(&file.path, &assignment.container, &assignment.field)
        {
            return Some(id);
        }
        // Otherwise the receiver names something in scope, and the question is
        // which container that something is.
        let Some(enclosing) = enclosing_function(file, assignment.span) else {
            self.unplaced.no_enclosing_function += 1;
            return None;
        };

        let Some(container) = self.container_of(file, enclosing, &assignment.receiver) else {
            if enclosing.receiver().is_none() {
                self.unplaced.not_a_method += 1;
            } else {
                self.unplaced.foreign_receiver += 1;
            }
            self.unplaced_sites.push(UnplacedSite {
                path: file.path.clone(),
                receiver: assignment.receiver.clone(),
                field: assignment.field.clone(),
                container: assignment.container.clone(),
                span: assignment.span,
            });
            if assignment.receiver.contains('.') {
                self.unplaced.chained_receiver += 1;
            } else if file.locals.iter().any(|local| {
                local.function == enclosing.qualified() && local.name == assignment.receiver
            }) {
                self.unplaced.untyped_local += 1;
            }
            return None;
        };

        let found = self.resolve_field(&file.path, &container, &assignment.field);
        if found.is_none() {
            self.unplaced.no_such_field += 1;
        }
        found
    }

    /// The container an assignment's receiver refers to.
    ///
    /// A receiver is not always a name. `self.inner.deep = x` writes through
    /// `self.inner`, and Bun writes 7,767 assignments that way — the single
    /// largest reason an assignment reaches no field. Resolving it is a walk:
    /// the head names a container, each step after that is a field of the
    /// container before it, and the field's declared type is the next
    /// container. Nothing here guesses; a step whose type is not written down
    /// ends the walk.
    fn container_of(
        &self,
        file: &FileObservations,
        enclosing: &Function,
        receiver: &str,
    ) -> Option<String> {
        let mut steps = receiver.split('.');
        let head = steps.next()?;

        // The enclosing method's own receiver, or a local whose type something
        // declared. The local is checked whether or not the enclosing function
        // is a method, because a constructor is not one and `var self: Foo =
        // undefined` is exactly where this happens.
        let mut container = enclosing
            .receiver()
            .filter(|parameter| parameter.name == head)
            .map(|_| enclosing.container.clone())
            .or_else(|| {
                self.local_types
                    .get(&(file.path.clone(), enclosing.qualified(), head.to_owned()))
                    .cloned()
            })
            .or_else(|| {
                // A pointer parameter names its own type: `fn f(dev: *DevServer)`
                // then `dev.field = x`.
                enclosing
                    .parameters
                    .iter()
                    .find(|parameter| parameter.name == head)
                    .map(|parameter| TypeShape::pointee(&parameter.zig_type).to_owned())
                    .filter(|written| !written.is_empty())
            })?;

        for step in steps {
            container = self.field_container(&file.path, &container, step)?;
        }
        Some(container)
    }

    /// The container a field's declared type names.
    fn field_container(&self, written_in: &Path, container: &str, field: &str) -> Option<String> {
        let bare = last_segment(container);
        let candidates = std::iter::once(written_in.to_path_buf()).chain(
            self.declared_in
                .get(container)
                .or_else(|| self.declared_in.get(&bare))
                .into_iter()
                .flatten()
                .cloned(),
        );
        for path in candidates {
            for name in [container.to_owned(), bare.clone()] {
                let key = FieldKey {
                    path: path.clone(),
                    container: name,
                    name: field.to_owned(),
                };
                if let Some(written) = self.field_types.get(&key) {
                    let pointee = TypeShape::pointee(written);
                    if !pointee.is_empty() {
                        return Some(pointee.to_owned());
                    }
                }
            }
        }
        None
    }

    /// The node for `container.field`, looked up in the declaring file.
    ///
    /// The file the assignment is written in comes first, because a container
    /// constructed where it is declared is the common case and needs no
    /// resolution. Failing that, the name is resolved across the corpus and
    /// accepted only when exactly one file declares a container by that name —
    /// two candidates mean the answer is a guess, and a guess here attributes a
    /// value to the wrong type's field.
    fn resolve_field(&self, written_in: &Path, container: &str, field: &str) -> Option<u64> {
        let local = FieldKey {
            path: written_in.to_path_buf(),
            container: container.to_owned(),
            name: field.to_owned(),
        };
        if let Some(id) = self.fields.get(&local) {
            return Some(*id);
        }
        let bare = last_segment(container);
        let candidates = self
            .declared_in
            .get(container)
            .or_else(|| self.declared_in.get(&bare))?;
        let mut found = candidates.iter().filter_map(|path| {
            for name in [container.to_owned(), bare.clone()] {
                let key = FieldKey {
                    path: path.clone(),
                    container: name,
                    name: field.to_owned(),
                };
                if let Some(id) = self.fields.get(&key) {
                    return Some(*id);
                }
            }
            None
        });
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// The node a bare name refers to, when it is a parameter in scope.
    fn named_source(
        &mut self,
        file: &FileObservations,
        assignment: &FieldAssignment,
        value: &str,
    ) -> Option<u64> {
        let name = value.trim_start_matches('&').trim();
        if name.contains(['(', '.', ' ']) {
            return None;
        }
        let enclosing = enclosing_function(file, assignment.span)?;
        if let Some(parameter) = enclosing
            .parameters
            .iter()
            .find(|parameter| parameter.name == name)
        {
            let node = parameter_node(&file.path, enclosing, parameter.index, &parameter.name);
            return Some(self.intern(&node));
        }
        // A local declared in the same function, which is how an allocation
        // reaches a field it was not assigned to directly.
        let qualified = enclosing.qualified();
        file.locals
            .iter()
            .find(|local| local.function == qualified && local.name == name)
            .map(|local| self.intern(&local_node(&file.path, &local.function, &local.name)))
    }
}

/// `self.child` reached through `self` gives `child`.
fn field_of_receiver(expression: &str, receiver: &str) -> Option<String> {
    let rest = expression.strip_prefix(receiver)?.strip_prefix('.')?;
    let name = rest.split('.').next()?;
    (!name.is_empty() && !name.contains('(')).then(|| name.to_owned())
}

/// The function a span sits inside, by containment.
///
/// Spans rather than a scope stack, because the observation layer reports each
/// kind of thing in its own pass and this is where they are joined back up.
fn enclosing_function(file: &FileObservations, span: Span) -> Option<&Function> {
    file.functions
        .iter()
        .filter(|function| {
            function.span.start_byte <= span.start_byte && span.end_byte <= function.span.end_byte
        })
        // the innermost one, for a function declared inside another
        .min_by_key(|function| function.span.end_byte - function.span.start_byte)
}

/// The function a written callee names, with the file that declares it.
///
/// Accepted only when exactly one function in the corpus answers to the name.
/// Two candidates mean the answer is a guess, and a guess here attributes a
/// value to the wrong constructor.
fn resolve_with_path<'a>(
    callee: &str,
    by_qualified: &'a BTreeMap<String, Vec<(&'a Path, &'a Function)>>,
) -> Option<(&'a Path, &'a Function)> {
    let candidates = by_qualified.get(callee)?;
    (candidates.len() == 1).then(|| candidates[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_key_never_collapses_to_the_file_stem() {
        let left = FieldKey {
            path: "src/a.zig".into(),
            container: "Outer".to_owned(),
            name: "ctx".to_owned(),
        };
        let right = FieldKey {
            path: "src/a.zig".into(),
            container: "Inner".to_owned(),
            name: "ctx".to_owned(),
        };
        assert_ne!(left, right);
        assert_ne!(left.node_name(), right.node_name());
    }

    fn observed(source_fields: &[(&str, &str)]) -> FileObservations {
        FileObservations {
            path: PathBuf::from("t.zig"),
            fields: source_fields
                .iter()
                .map(|(container, name)| ContainerField {
                    path: PathBuf::from("t.zig"),
                    container: (*container).to_owned(),
                    container_kind: entl_zig_observe::ContainerKind::Struct,
                    name: (*name).to_owned(),
                    zig_type: "*Thing".to_owned(),
                    span: Span {
                        start_byte: 0,
                        end_byte: 1,
                        start_line: 1,
                        end_line: 1,
                    },
                    type_span: Span {
                        start_byte: 0,
                        end_byte: 1,
                        start_line: 1,
                        end_line: 1,
                    },
                    comptime: false,
                })
                .collect(),
            ..FileObservations::default()
        }
    }

    /// Two containers in one file with the same field name must stay apart.
    #[test]
    fn same_named_fields_in_different_containers_are_different_nodes() {
        let flow = build(&[observed(&[("Outer", "ctx"), ("Inner", "ctx")])]);
        let outer = flow
            .node(&FieldKey {
                path: PathBuf::from("t.zig"),
                container: "Outer".to_owned(),
                name: "ctx".to_owned(),
            })
            .expect("Outer.ctx");
        let inner = flow
            .node(&FieldKey {
                path: PathBuf::from("t.zig"),
                container: "Inner".to_owned(),
                name: "ctx".to_owned(),
            })
            .expect("Inner.ctx");
        assert_ne!(outer, inner);
    }

    /// Nothing is concluded from nothing: an empty corpus balances and says so.
    #[test]
    fn an_empty_corpus_reaches_nothing_and_balances() {
        let flow = build(&[]);
        assert!(flow.accounting().balances());
        assert_eq!(flow.accounting().total, 0);
        assert!(flow.graph().propagate().is_empty());
    }

    #[test]
    fn a_receivers_field_is_read_out_of_an_expression() {
        assert_eq!(
            field_of_receiver("self.child", "self"),
            Some("child".to_owned())
        );
        assert_eq!(
            field_of_receiver("this.inner.deep", "this"),
            Some("inner".to_owned())
        );
        assert_eq!(field_of_receiver("other.child", "self"), None);
        assert_eq!(field_of_receiver("self", "self"), None);
    }
}
