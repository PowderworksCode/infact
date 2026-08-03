//! Following a library callable to the code that describes what it does.
//!
//! A public API is rarely where the work is. `Iterator::find` is a `try_fold`,
//! `itertools::counts` is a call to `counts_with_hasher`, and `filter_map` only
//! builds a `FilterMap` whose `next` runs later. Deriving a behavior therefore
//! means walking from the name a caller writes to the body that does the thing,
//! and the walk is the same walk in every language: follow a delegation, follow
//! a construction into the constructed type's contract method, stop when the
//! form describes work.
//!
//! Nothing here parses anything. A frontend supplies each callable already
//! normalized, and gets back the form plus the chain that was followed, which is
//! what its evidence spans are recorded from. That division is what lets one
//! walk serve Rust and TypeScript: the languages disagree about how a function
//! is spelled and agree completely about what following one means.

use std::collections::{BTreeMap, BTreeSet};

use infact_normalize::{Form, MAXIMUM_FORM_DEPTH};

/// How many delegating wrappers to follow before giving up.
pub const MAX_DELEGATION_DEPTH: usize = 4;

/// Which source a callable was written in.
///
/// An opaque index rather than a path, because the walk only ever asks whether
/// two callables were written in the same place. Handing it paths would tie it
/// to a filesystem it has no other reason to know about.
pub type SourceId = usize;

/// One function in a library, as far as derivation cares.
pub struct LibraryCallable {
    /// The name the language gives it.
    pub name: String,
    /// The type or trait it is written inside, when there is one.
    pub container: Option<String>,
    /// Where it was written.
    pub source: SourceId,
    /// Its normalized body, or `None` when it has none.
    ///
    /// A declaration without a body is not the same as a function that could
    /// not be found, and a frontend that dropped it would report the wrong one.
    pub form: Option<Form>,
}

/// Why a callable yielded no behavior.
///
/// Most callables in a library describe nothing comparable, so refusing is the
/// normal outcome and each reason is worth counting separately: they say
/// different things about where coverage is actually bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The name resolved to nothing, or to more than one thing.
    NoImplementation,
    /// The implementation is a declaration with no body.
    NoBody,
    /// The implementation describes neither a traversal nor a decision.
    NotComparable,
    /// The implementation nests far enough to be a subsystem.
    TooDeep(u32),
}

/// What a walk arrived at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    /// The form of the body that actually does the work.
    pub form: Form,
    /// Every callable the walk passed through, starting with the one asked for.
    ///
    /// The frontend turns these into implementation evidence. The order is the
    /// order they were followed in, so the first is the API a caller names and
    /// the last is where the behavior is written.
    pub chain: Vec<usize>,
}

/// A library's callables, indexed for the questions the walk asks.
pub struct Library<'a> {
    callables: &'a [LibraryCallable],
    /// Which sources declare each named type.
    ///
    /// A bare type name only identifies a type when the library gives it to
    /// one. The standard library has four distinct types named `Cursor`, and
    /// before this was consulted they pooled into one candidate set and handed
    /// each other's behaviors out.
    declarations: BTreeMap<String, BTreeSet<SourceId>>,
    /// The methods this language requires of the types it can follow into.
    ///
    /// An iterator's `next` is its whole contract; its `fold` and `size_hint`
    /// are specializations of it. Which names those are is the one thing about
    /// the walk that differs by language, so the frontend states them and
    /// earlier entries outrank later ones.
    contract_methods: &'a [&'a str],
}

impl<'a> Library<'a> {
    pub fn new(
        callables: &'a [LibraryCallable],
        declarations: BTreeMap<String, BTreeSet<SourceId>>,
        contract_methods: &'a [&'a str],
    ) -> Self {
        Self {
            callables,
            declarations,
            contract_methods,
        }
    }

    /// Resolve a name to one implementation.
    ///
    /// Preferring the container narrows an ambiguous name; without one, only a
    /// name the library uses exactly once can be resolved by syntax alone.
    /// `exclude` drops the function doing the delegating: a wrapper and the
    /// helper it forwards to routinely share a name across a trait and a
    /// module, and a wrapper is never its own implementation.
    fn resolve(
        &self,
        name: &str,
        container: Option<&str>,
        exclude: Option<usize>,
    ) -> Option<usize> {
        let candidates = || {
            self.callables
                .iter()
                .enumerate()
                .filter(move |(index, callable)| callable.name == name && exclude != Some(*index))
        };
        if let Some(container) = container {
            let mut qualified = candidates()
                .filter(|(_, callable)| callable.container.as_deref() == Some(container));
            if let Some((first, _)) = qualified.next()
                && qualified.next().is_none()
            {
                return Some(first);
            }
        }
        let mut matching = candidates();
        let (first, _) = matching.next()?;
        matching.next().is_none().then_some(first)
    }

    /// The method that carries a type's behavior.
    ///
    /// A type's work is spread across its implementations and most of those
    /// methods are bookkeeping. Prefer the one the language requires; failing
    /// that, take whichever describes the most work. This is a good enough
    /// answer, because a finding here is a prompt to look rather than a proof.
    ///
    /// `within` is the source the construction was written in, and it is what
    /// keeps this honest: `type_name` is a BARE name, so matching on it alone
    /// pools every type in the library that answers to it.
    fn principal_method(&self, type_name: &str, within: SourceId) -> Option<usize> {
        let named = || {
            self.callables
                .iter()
                .enumerate()
                .filter(|(_, callable)| callable.container.as_deref() == Some(type_name))
        };
        // A type is usually implemented beside its construction, and that
        // source answers the question without any name resolution at all.
        let mut candidates = named()
            .filter(|(_, callable)| callable.source == within)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            // Otherwise the name has to identify the type on its own, which it
            // does only when the library declares it once. A trait in one file
            // returning an adaptor declared in its own module is the common
            // shape and stays followable; two declarations of one name are
            // refused, because picking either attributes one type's behavior to
            // another and nothing downstream would show it.
            if self.declarations.get(type_name).map_or(0, BTreeSet::len) != 1 {
                return None;
            }
            candidates = named().collect();
        }
        candidates
            .into_iter()
            .filter_map(|(index, callable)| {
                let form = callable.form.as_ref()?;
                // The method the language requires wins even when its body is a
                // single call, because that call is followed afterwards.
                // Demanding that it already describe work leaves a bulk
                // specialization to win by default, which describes something
                // the caller never asked for.
                let principal = self.contract_methods.contains(&callable.name.as_str());
                let works = form.describes_work();
                // `next` is the contract; `next_back` and the rest are
                // specializations of it. Ranking by size alone let `next_back`
                // win whenever `next` was a one-line delegation, which is
                // exactly when `next` is the method worth following.
                let standing = self
                    .contract_methods
                    .iter()
                    .position(|candidate| *candidate == callable.name)
                    .map_or(0, |rank| self.contract_methods.len() - rank);
                // Only a method the language requires can stand for a type's
                // behavior. Admitting any function that merely does work meant
                // a type with no contract method at all handed back whichever
                // of its methods happened to be largest.
                principal.then_some(((standing, works, form.size()), index))
            })
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, index)| index)
    }

    /// Walk from a named callable to the body that describes what it does.
    ///
    /// `container` is what the caller's path qualified the name with, when it
    /// had one; it narrows an ambiguous name and is not required.
    pub fn derive(&self, name: &str, container: Option<&str>) -> Result<Derived, Refusal> {
        let mut current = self
            .resolve(name, container, None)
            .ok_or(Refusal::NoImplementation)?;
        let mut chain = vec![current];
        let mut form = self.form(current)?;
        // Whether derivation ever stepped into a constructed type's contract
        // method. It is sticky: once inside a lazy adaptor's `next`, whatever
        // further delegation reaches is still one step of that adaptor.
        // `Iterator::filter_map` goes filter_map -> next -> find_map, and the
        // liftable form only appears at find_map, a step after the
        // construction.
        let mut inside_one_step = false;

        // Follow the implementation until it describes actual work, by
        // whichever route leads there: a wrapper delegating to a helper, or a
        // callable that only builds the type whose implementation does the work
        // later.
        for _ in 0..MAX_DELEGATION_DEPTH {
            if form.describes_work() {
                break;
            }
            let container = self.callables[current].container.as_deref();
            let delegated = delegation_target(&form)
                .and_then(|target| self.resolve(target, container, Some(current)));
            // Following a construction into the constructed type's contract
            // method is the one route that lands on a lazy adaptor, and the
            // only one that licenses the one-step lift below.
            let built_into = if delegated.is_some() {
                None
            } else {
                constructed_type(&form)
                    .and_then(|built| resolve_self(built, container))
                    .and_then(|built| self.principal_method(built, self.callables[current].source))
            };
            inside_one_step |= built_into.is_some();
            let Some(next) = delegated.or(built_into) else {
                break;
            };
            // a trait wrapper and the free function it forwards to commonly
            // share a name, so identity rather than name decides whether this
            // is a cycle
            if chain.contains(&next) {
                break;
            }
            chain.push(next);
            current = next;
            form = self.form(current)?;
        }

        // A combinator does not do its work where it is called. `map_into` only
        // builds a `MapInto`, and the behavior lives in that type's `Iterator`
        // implementation, which runs later. When a callable just constructs
        // something, the type it constructs is where to look.
        if !form.describes_work()
            && let Some(constructed) = constructed_type(&form)
            && let Some(constructed) =
                resolve_self(constructed, self.callables[current].container.as_deref())
            && let Some(implementing) =
                self.principal_method(constructed, self.callables[current].source)
        {
            form = self.form(implementing)?;
            inside_one_step = true;
            chain.push(implementing);
        }

        // One step of a lazy adaptor stands for the whole operation, and only a
        // derivation that went through a construction has earned that reading.
        if inside_one_step && let Some(lifted) = form.lifted_from_one_step() {
            form = lifted;
        }

        if !form.is_comparable() {
            return Err(Refusal::NotComparable);
        }
        let depth = form.depth();
        if depth > MAXIMUM_FORM_DEPTH {
            return Err(Refusal::TooDeep(depth));
        }
        Ok(Derived { form, chain })
    }

    fn form(&self, index: usize) -> Result<Form, Refusal> {
        self.callables[index].form.clone().ok_or(Refusal::NoBody)
    }
}

/// The name a qualified path ends in.
#[must_use]
pub fn leaf_name(callable_path: &str) -> &str {
    callable_path.rsplit("::").next().unwrap_or(callable_path)
}

/// The trait or type a callable path is qualified by, when it has one.
///
/// A leading-uppercase segment is a type name in every language that has this
/// convention, and a lowercase one is a module. Getting this wrong only widens
/// a candidate set, which the resolver then refuses as ambiguous.
#[must_use]
pub fn container_name(callable_path: &str) -> Option<&str> {
    let mut segments = callable_path.rsplit("::");
    segments.next()?;
    segments
        .next()
        .filter(|segment| segment.starts_with(|first: char| first.is_ascii_uppercase()))
}

/// Whether a form is nothing but a call to somewhere else.
///
/// A public API is frequently a one-line wrapper: `counts` exists to call
/// `counts_with_hasher` with a default. The wrapper describes no behavior of
/// its own, so derivation follows it. This is a shape test, not a list of known
/// wrappers.
#[must_use]
pub fn delegation_target(form: &Form) -> Option<&str> {
    match form {
        Form::Method { name, .. } => Some(name),
        Form::Call { callee, .. } => match callee.as_ref() {
            Form::Path(path) => Some(leaf_name(path)),
            _ => None,
        },
        Form::Return(inner) => delegation_target(inner),
        Form::Sequence(parts) => match parts.as_slice() {
            [only] => delegation_target(only),
            _ => None,
        },
        _ => None,
    }
}

/// The type a form does nothing but construct.
#[must_use]
pub fn constructed_type(form: &Form) -> Option<&str> {
    match form {
        Form::Construct(name) => Some(name),
        Form::Return(inner) => constructed_type(inner),
        Form::Sequence(parts) => match parts.as_slice() {
            [only] => constructed_type(only),
            _ => None,
        },
        // `MapInto { iter: self }` and similar struct literals reach here as
        // opaque syntax; the constructed type is whatever the parts construct
        _ => form.children().into_iter().find_map(constructed_type),
    }
}

/// The type a name refers to, with a self-reference read as the type it stands
/// for.
///
/// `Self` is not a type name: it means whichever type the surrounding
/// implementation is for. Taken literally it matches every such block in the
/// library, and fifty-four callables were linked to implementations belonging to
/// unrelated types. Where the enclosing type is unknown there is no answer, and
/// none is better than an arbitrary one.
#[must_use]
pub fn resolve_self<'a>(name: &'a str, container: Option<&'a str>) -> Option<&'a str> {
    if name == "Self" {
        return container;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use infact_normalize::{Direction, Pattern};

    fn callable(
        name: &str,
        container: Option<&str>,
        source: SourceId,
        form: Option<Form>,
    ) -> LibraryCallable {
        LibraryCallable {
            name: name.to_owned(),
            container: container.map(str::to_owned),
            source,
            form,
        }
    }

    /// A traversal, which is the smallest thing that `describes_work`.
    fn work() -> Form {
        Form::Traverse {
            sequence: Box::new(Form::Free(0)),
            item: Box::new(Pattern::Binding(0)),
            body: Box::new(Form::Method {
                receiver: Box::new(Form::Local(0)),
                name: "push".to_owned(),
                arguments: Vec::new(),
            }),
            direction: Direction::Forward,
        }
    }

    fn calls(name: &str) -> Form {
        Form::Method {
            receiver: Box::new(Form::Free(0)),
            name: name.to_owned(),
            arguments: Vec::new(),
        }
    }

    #[test]
    fn a_wrapper_is_followed_to_the_helper_that_does_the_work() {
        let callables = vec![
            callable("counts", None, 0, Some(calls("counts_with_hasher"))),
            callable("counts_with_hasher", None, 0, Some(work())),
        ];
        let library = Library::new(&callables, BTreeMap::new(), &["next"]);
        let derived = library.derive("counts", None).unwrap();
        assert_eq!(derived.chain, vec![0, 1]);
        assert_eq!(derived.form, work());
    }

    /// A wrapper is not its own implementation.
    ///
    /// A trait method and the free function it forwards to routinely share a
    /// name, and resolving the delegation back to the caller would report a
    /// one-line wrapper as a cycle and stop there.
    #[test]
    fn a_delegation_does_not_resolve_to_the_caller() {
        let callables = vec![
            callable("find", Some("Iterator"), 0, Some(calls("find"))),
            callable("find", None, 0, Some(work())),
        ];
        let library = Library::new(&callables, BTreeMap::new(), &["next"]);
        let derived = library.derive("find", Some("Iterator")).unwrap();
        assert_eq!(derived.chain, vec![0, 1]);
    }

    /// A name the library uses twice cannot be resolved by syntax alone.
    #[test]
    fn an_ambiguous_name_resolves_to_nothing() {
        let callables = vec![
            callable("next", Some("Keys"), 0, Some(work())),
            callable("next", Some("Values"), 1, Some(work())),
        ];
        let library = Library::new(&callables, BTreeMap::new(), &["next"]);
        assert_eq!(library.derive("next", None), Err(Refusal::NoImplementation));
    }

    /// Following a construction needs the type to be declared exactly once.
    ///
    /// Two types with one name pooled into a single candidate set is how
    /// `LinkedList::cursor_back` was handed a `BTreeMap` cursor's behavior.
    #[test]
    fn a_name_two_types_answer_to_is_not_followed_across_files() {
        let callables = vec![
            callable("iter", None, 0, Some(Form::Construct("Cursor".to_owned()))),
            callable("next", Some("Cursor"), 1, Some(work())),
        ];
        let ambiguous = BTreeMap::from([("Cursor".to_owned(), BTreeSet::from([1, 2]))]);
        let library = Library::new(&callables, ambiguous, &["next"]);
        assert_eq!(library.derive("iter", None), Err(Refusal::NotComparable));

        let declared_once = BTreeMap::from([("Cursor".to_owned(), BTreeSet::from([1]))]);
        let library = Library::new(&callables, declared_once, &["next"]);
        assert_eq!(library.derive("iter", None).unwrap().chain, vec![0, 1]);
    }

    /// A type is followed into the file it was constructed in without needing
    /// to be named unambiguously, because that file already answers which one
    /// it is.
    #[test]
    fn a_construction_is_followed_within_one_file() {
        let callables = vec![
            callable("iter", None, 0, Some(Form::Construct("Cursor".to_owned()))),
            callable("next", Some("Cursor"), 0, Some(work())),
            callable("next", Some("Cursor"), 1, Some(work())),
        ];
        let library = Library::new(&callables, BTreeMap::new(), &["next"]);
        assert_eq!(library.derive("iter", None).unwrap().chain, vec![0, 1]);
    }

    /// Only a method the language requires can stand for a type's behavior.
    #[test]
    fn a_type_with_no_contract_method_is_not_followed_into() {
        let callables = vec![
            callable("from_raw", None, 0, Some(Form::Construct("Arc".to_owned()))),
            callable("clone_from_slice", Some("Arc"), 0, Some(work())),
        ];
        let library = Library::new(&callables, BTreeMap::new(), &["next"]);
        assert_eq!(
            library.derive("from_raw", None),
            Err(Refusal::NotComparable)
        );
    }

    #[test]
    fn a_declaration_with_no_body_is_told_apart_from_a_missing_one() {
        let callables = vec![callable("find", None, 0, None)];
        let library = Library::new(&callables, BTreeMap::new(), &["next"]);
        assert_eq!(library.derive("find", None), Err(Refusal::NoBody));
        assert_eq!(
            library.derive("absent", None),
            Err(Refusal::NoImplementation)
        );
    }

    #[test]
    fn a_container_is_only_read_off_a_path_that_names_a_type() {
        assert_eq!(
            container_name("itertools::Itertools::counts"),
            Some("Itertools")
        );
        assert_eq!(container_name("itertools::counts"), None);
        assert_eq!(container_name("counts"), None);
        assert_eq!(leaf_name("itertools::Itertools::counts"), "counts");
    }
}
