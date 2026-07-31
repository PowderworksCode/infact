use std::path::Path;

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use infact_core::{
    DERIVED_LIBRARY_BEHAVIOR_SCHEMA, DerivedLibraryBehavior, ExternalCatalog,
    ImplementationEvidence, NormalizedBehavior, NormalizedOperation, NormalizedValue,
    SortComparison, SortStability, SourceSpan,
};
use tree_sitter::Node;

use crate::{Error, Result};

const COUNTS: &str = "itertools::Itertools::counts";
const COUNTS_WITH_HASHER: &str = "itertools::Itertools::counts_with_hasher";
const COUNTS_BY: &str = "itertools::Itertools::counts_by";
const COUNTS_BY_WITH_HASHER: &str = "itertools::Itertools::counts_by_with_hasher";
const GROUP_MAP: &str = "itertools::Itertools::into_group_map";
const GROUP_MAP_WITH_HASHER: &str = "itertools::group_map::into_group_map_with_hasher";
const GROUP_MAP_BY: &str = "itertools::Itertools::into_group_map_by";
const GROUP_MAP_BY_WITH_HASHER: &str = "itertools::group_map::into_group_map_by_with_hasher";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortedKind {
    Stable,
    StableBy,
    StableByKey,
    Unstable,
    UnstableBy,
    UnstableByKey,
}

impl SortedKind {
    pub(crate) const ALL: [Self; 6] = [
        Self::Stable,
        Self::StableBy,
        Self::StableByKey,
        Self::Unstable,
        Self::UnstableBy,
        Self::UnstableByKey,
    ];

    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Stable => "itertools::Itertools::sorted",
            Self::StableBy => "itertools::Itertools::sorted_by",
            Self::StableByKey => "itertools::Itertools::sorted_by_key",
            Self::Unstable => "itertools::Itertools::sorted_unstable",
            Self::UnstableBy => "itertools::Itertools::sorted_unstable_by",
            Self::UnstableByKey => "itertools::Itertools::sorted_unstable_by_key",
        }
    }

    pub(crate) fn method(self) -> &'static str {
        match self {
            Self::Stable => "sort",
            Self::StableBy => "sort_by",
            Self::StableByKey => "sort_by_key",
            Self::Unstable => "sort_unstable",
            Self::UnstableBy => "sort_unstable_by",
            Self::UnstableByKey => "sort_unstable_by_key",
        }
    }

    fn stability(self) -> SortStability {
        match self {
            Self::Stable | Self::StableBy | Self::StableByKey => SortStability::Stable,
            Self::Unstable | Self::UnstableBy | Self::UnstableByKey => SortStability::Unstable,
        }
    }

    fn comparison(self) -> SortComparison {
        match self {
            Self::Stable | Self::Unstable => SortComparison::Natural,
            Self::StableBy | Self::UnstableBy => SortComparison::Comparator {
                function: value("parameter:cmp"),
            },
            Self::StableByKey | Self::UnstableByKey => SortComparison::Key {
                function: value("parameter:f"),
            },
        }
    }
}

pub fn derive_behavior(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
    callable_path: &str,
) -> Result<DerivedLibraryBehavior> {
    match callable_path {
        COUNTS => derive_counts(source_root, parsers, catalog),
        COUNTS_BY => derive_counts_by(source_root, parsers, catalog),
        GROUP_MAP => derive_group_map(source_root, parsers, catalog),
        GROUP_MAP_BY => derive_group_map_by(source_root, parsers, catalog),
        path if SortedKind::ALL.iter().any(|kind| kind.path() == path) => {
            let kind = SortedKind::ALL
                .into_iter()
                .find(|kind| kind.path() == path)
                .expect("guard found a sorting method");
            derive_sorted(source_root, parsers, catalog, kind)
        }
        _ => Err(Error::UnsupportedDerivation {
            callable: callable_path.to_owned(),
        }),
    }
}

fn derive_sorted(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
    kind: SortedKind,
) -> Result<DerivedLibraryBehavior> {
    require_callable(catalog, kind.path(), |signature| {
        crate::is_sorted_signature(signature, kind)
    })?;
    let parsed = parse_repository(source_root, parsers)?;
    let (file, function) = required_method(
        &parsed.files,
        kind.path().rsplit("::").next().unwrap(),
        kind.path(),
    )?;
    let body = required_body(function, kind.path())?;
    let program = normalize_sorted(body, &file.source, kind).ok_or_else(|| {
        Error::UnsupportedImplementation {
            callable: kind.path().to_owned(),
            reason: "body is not Vec::from_iter, the expected slice sort, and into_iter".to_owned(),
        }
    })?;
    derived(
        catalog,
        kind.path(),
        program,
        vec![evidence(file, function, kind.path())?],
    )
}

fn derive_counts(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
) -> Result<DerivedLibraryBehavior> {
    let callable_path = COUNTS;
    let callable = catalog
        .callables
        .iter()
        .find(|callable| callable.path == callable_path)
        .ok_or_else(|| Error::MissingCallable {
            callable: callable_path.to_owned(),
        })?;
    if !crate::is_counts_signature(&callable.signature) {
        return Err(Error::IncompatibleCallable {
            callable: callable_path.to_owned(),
        });
    }

    let parsed = parse_repository(source_root, parsers)?;
    let (entry_file, entry) =
        find_function(&parsed.files, "counts").ok_or_else(|| Error::MissingImplementation {
            callable: callable_path.to_owned(),
        })?;
    let entry_body =
        entry
            .child_by_field_name("body")
            .ok_or_else(|| Error::UnsupportedImplementation {
                callable: callable_path.to_owned(),
                reason: "entry method has no body".to_owned(),
            })?;
    if !calls_method(entry_body, &entry_file.source, "counts_with_hasher") {
        return Err(Error::UnsupportedImplementation {
            callable: callable_path.to_owned(),
            reason: "entry method does not delegate to counts_with_hasher".to_owned(),
        });
    }

    let (helper_file, helper) =
        find_function(&parsed.files, "counts_with_hasher").ok_or_else(|| {
            Error::MissingImplementation {
                callable: COUNTS_WITH_HASHER.to_owned(),
            }
        })?;
    let helper_body =
        helper
            .child_by_field_name("body")
            .ok_or_else(|| Error::UnsupportedImplementation {
                callable: COUNTS_WITH_HASHER.to_owned(),
                reason: "helper method has no body".to_owned(),
            })?;
    let program = normalize_counts_helper(helper_body, &helper_file.source).ok_or_else(|| {
        Error::UnsupportedImplementation {
            callable: COUNTS_WITH_HASHER.to_owned(),
            reason:
                "body is not a map initializer, iterator traversal, entry increment, and map return"
                    .to_owned(),
        }
    })?;

    Ok(DerivedLibraryBehavior {
        schema: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
        callable_package: catalog.package.clone(),
        callable_version: catalog.version.clone(),
        callable_path: callable_path.to_owned(),
        catalog_sha256: catalog.source_sha256.clone(),
        implementation: vec![
            evidence(entry_file, entry, COUNTS)?,
            evidence(helper_file, helper, COUNTS_WITH_HASHER)?,
        ],
        program,
    })
}

fn derive_counts_by(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
) -> Result<DerivedLibraryBehavior> {
    require_callable(catalog, COUNTS_BY, crate::is_counts_by_signature)?;
    let parsed = parse_repository(source_root, parsers)?;
    let (entry_file, entry) = required_function(&parsed.files, "counts_by", COUNTS_BY)?;
    require_call(entry_file, entry, COUNTS_BY, "counts_by_with_hasher")?;
    let (mapped_file, mapped) = required_function(
        &parsed.files,
        "counts_by_with_hasher",
        COUNTS_BY_WITH_HASHER,
    )?;
    require_call(
        mapped_file,
        mapped,
        COUNTS_BY_WITH_HASHER,
        "counts_with_hasher",
    )?;
    require_call(mapped_file, mapped, COUNTS_BY_WITH_HASHER, "map")?;
    let (base_file, base) =
        required_function(&parsed.files, "counts_with_hasher", COUNTS_WITH_HASHER)?;
    let base_body = required_body(base, COUNTS_WITH_HASHER)?;
    if normalize_counts_helper(base_body, &base_file.source).is_none() {
        return unsupported(COUNTS_WITH_HASHER, "base histogram body did not normalize");
    }
    derived(
        catalog,
        COUNTS_BY,
        counts_by_program(),
        vec![
            evidence(entry_file, entry, COUNTS_BY)?,
            evidence(mapped_file, mapped, COUNTS_BY_WITH_HASHER)?,
            evidence(base_file, base, COUNTS_WITH_HASHER)?,
        ],
    )
}

fn derive_group_map(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
) -> Result<DerivedLibraryBehavior> {
    require_callable(catalog, GROUP_MAP, crate::is_group_map_signature)?;
    let parsed = parse_repository(source_root, parsers)?;
    let (entry_file, entry) = required_function(&parsed.files, "into_group_map", GROUP_MAP)?;
    require_named_call(entry_file, entry, GROUP_MAP, "into_group_map_with_hasher")?;
    let (base_file, base) = required_function_in(
        &parsed.files,
        Path::new("src/group_map.rs"),
        "into_group_map_with_hasher",
        GROUP_MAP_WITH_HASHER,
    )?;
    let base_body = required_body(base, GROUP_MAP_WITH_HASHER)?;
    if normalize_group_map_helper(base_body, &base_file.source).is_none() {
        return unsupported(GROUP_MAP_WITH_HASHER, "grouping body did not normalize");
    }
    derived(
        catalog,
        GROUP_MAP,
        group_map_program(),
        vec![
            evidence(entry_file, entry, GROUP_MAP)?,
            evidence(base_file, base, GROUP_MAP_WITH_HASHER)?,
        ],
    )
}

fn derive_group_map_by(
    source_root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalog: &ExternalCatalog,
) -> Result<DerivedLibraryBehavior> {
    require_callable(catalog, GROUP_MAP_BY, crate::is_group_map_by_signature)?;
    let parsed = parse_repository(source_root, parsers)?;
    let (entry_file, entry) = required_function(&parsed.files, "into_group_map_by", GROUP_MAP_BY)?;
    require_named_call(
        entry_file,
        entry,
        GROUP_MAP_BY,
        "into_group_map_by_with_hasher",
    )?;
    let (mapped_file, mapped) = required_function_in(
        &parsed.files,
        Path::new("src/group_map.rs"),
        "into_group_map_by_with_hasher",
        GROUP_MAP_BY_WITH_HASHER,
    )?;
    require_named_call(
        mapped_file,
        mapped,
        GROUP_MAP_BY_WITH_HASHER,
        "into_group_map_with_hasher",
    )?;
    require_call(mapped_file, mapped, GROUP_MAP_BY_WITH_HASHER, "map")?;
    let (base_file, base) = required_function_in(
        &parsed.files,
        Path::new("src/group_map.rs"),
        "into_group_map_with_hasher",
        GROUP_MAP_WITH_HASHER,
    )?;
    let base_body = required_body(base, GROUP_MAP_WITH_HASHER)?;
    if normalize_group_map_helper(base_body, &base_file.source).is_none() {
        return unsupported(GROUP_MAP_WITH_HASHER, "grouping body did not normalize");
    }
    derived(
        catalog,
        GROUP_MAP_BY,
        group_map_by_program(),
        vec![
            evidence(entry_file, entry, GROUP_MAP_BY)?,
            evidence(mapped_file, mapped, GROUP_MAP_BY_WITH_HASHER)?,
            evidence(base_file, base, GROUP_MAP_WITH_HASHER)?,
        ],
    )
}

fn require_callable(
    catalog: &ExternalCatalog,
    path: &str,
    compatible: impl FnOnce(&infact_core::CallableSignature) -> bool,
) -> Result<()> {
    let callable = catalog
        .callables
        .iter()
        .find(|callable| callable.path == path)
        .ok_or_else(|| Error::MissingCallable {
            callable: path.to_owned(),
        })?;
    if !compatible(&callable.signature) {
        return Err(Error::IncompatibleCallable {
            callable: path.to_owned(),
        });
    }
    Ok(())
}

fn derived(
    catalog: &ExternalCatalog,
    callable_path: &str,
    program: NormalizedBehavior,
    implementation: Vec<ImplementationEvidence>,
) -> Result<DerivedLibraryBehavior> {
    Ok(DerivedLibraryBehavior {
        schema: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
        callable_package: catalog.package.clone(),
        callable_version: catalog.version.clone(),
        callable_path: callable_path.to_owned(),
        catalog_sha256: catalog.source_sha256.clone(),
        implementation,
        program,
    })
}

pub(crate) fn counts_program() -> NormalizedBehavior {
    NormalizedBehavior {
        operations: vec![
            NormalizedOperation::CreateMap {
                output: value("map"),
            },
            NormalizedOperation::Iterate {
                input: value("input"),
                item: value("item"),
                body: vec![NormalizedOperation::IncrementMapEntry {
                    map: value("map"),
                    key: value("item"),
                    amount: 1,
                }],
            },
            NormalizedOperation::Return {
                value: value("map"),
            },
        ],
    }
}

pub(crate) fn counts_by_program() -> NormalizedBehavior {
    NormalizedBehavior {
        operations: vec![
            NormalizedOperation::CreateMap {
                output: value("map"),
            },
            NormalizedOperation::Iterate {
                input: value("input"),
                item: value("item"),
                body: vec![
                    NormalizedOperation::Apply {
                        function: value("parameter:f"),
                        input: value("item"),
                        output: value("key"),
                    },
                    NormalizedOperation::IncrementMapEntry {
                        map: value("map"),
                        key: value("key"),
                        amount: 1,
                    },
                ],
            },
            NormalizedOperation::Return {
                value: value("map"),
            },
        ],
    }
}

pub(crate) fn group_map_program() -> NormalizedBehavior {
    NormalizedBehavior {
        operations: vec![
            NormalizedOperation::CreateMap {
                output: value("map"),
            },
            NormalizedOperation::Iterate {
                input: value("input"),
                item: value("item"),
                body: vec![
                    NormalizedOperation::DestructurePair {
                        input: value("item"),
                        first: value("key"),
                        second: value("value"),
                    },
                    NormalizedOperation::PushMapEntry {
                        map: value("map"),
                        key: value("key"),
                        value: value("value"),
                    },
                ],
            },
            NormalizedOperation::Return {
                value: value("map"),
            },
        ],
    }
}

pub(crate) fn group_map_by_program() -> NormalizedBehavior {
    NormalizedBehavior {
        operations: vec![
            NormalizedOperation::CreateMap {
                output: value("map"),
            },
            NormalizedOperation::Iterate {
                input: value("input"),
                item: value("item"),
                body: vec![
                    NormalizedOperation::Apply {
                        function: value("parameter:f"),
                        input: value("item"),
                        output: value("key"),
                    },
                    NormalizedOperation::PushMapEntry {
                        map: value("map"),
                        key: value("key"),
                        value: value("item"),
                    },
                ],
            },
            NormalizedOperation::Return {
                value: value("map"),
            },
        ],
    }
}

pub(crate) fn sorted_program(kind: SortedKind) -> NormalizedBehavior {
    NormalizedBehavior {
        operations: vec![
            NormalizedOperation::CollectVec {
                input: value("input"),
                output: value("values"),
            },
            NormalizedOperation::Sort {
                value: value("values"),
                stability: kind.stability(),
                comparison: kind.comparison(),
            },
            NormalizedOperation::IntoIterator {
                input: value("values"),
                output: value("output"),
            },
            NormalizedOperation::Return {
                value: value("output"),
            },
        ],
    }
}

fn value(name: &str) -> NormalizedValue {
    NormalizedValue(name.to_owned())
}

fn find_function<'a>(
    files: &'a [ParsedFile],
    expected: &str,
) -> Option<(&'a ParsedFile, Node<'a>)> {
    for file in files {
        if file.pack.language().id != "rust" {
            continue;
        }
        let mut stack = vec![file.tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_item"
                && field_text(node, "name", &file.source) == Some(expected)
            {
                return Some((file, node));
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    None
}

fn find_method<'a>(files: &'a [ParsedFile], expected: &str) -> Option<(&'a ParsedFile, Node<'a>)> {
    for file in files {
        if file.pack.language().id != "rust" {
            continue;
        }
        let mut stack = vec![file.tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_item"
                && field_text(node, "name", &file.source) == Some(expected)
                && node
                    .child_by_field_name("parameters")
                    .and_then(|parameters| parameters.named_child(0))
                    .is_some_and(|parameter| parameter.kind() == "self_parameter")
            {
                return Some((file, node));
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    None
}

fn find_function_in<'a>(
    files: &'a [ParsedFile],
    path: &Path,
    expected: &str,
) -> Option<(&'a ParsedFile, Node<'a>)> {
    let file = files.iter().find(|file| file.path == path)?;
    find_function(std::slice::from_ref(file), expected)
}

fn required_function<'a>(
    files: &'a [ParsedFile],
    name: &str,
    callable: &str,
) -> Result<(&'a ParsedFile, Node<'a>)> {
    find_function(files, name).ok_or_else(|| Error::MissingImplementation {
        callable: callable.to_owned(),
    })
}

fn required_method<'a>(
    files: &'a [ParsedFile],
    name: &str,
    callable: &str,
) -> Result<(&'a ParsedFile, Node<'a>)> {
    find_method(files, name).ok_or_else(|| Error::MissingImplementation {
        callable: callable.to_owned(),
    })
}

fn required_function_in<'a>(
    files: &'a [ParsedFile],
    path: &Path,
    name: &str,
    callable: &str,
) -> Result<(&'a ParsedFile, Node<'a>)> {
    find_function_in(files, path, name).ok_or_else(|| Error::MissingImplementation {
        callable: callable.to_owned(),
    })
}

fn required_body<'a>(function: Node<'a>, callable: &str) -> Result<Node<'a>> {
    function
        .child_by_field_name("body")
        .ok_or_else(|| Error::UnsupportedImplementation {
            callable: callable.to_owned(),
            reason: "implementation has no body".to_owned(),
        })
}

fn require_call(file: &ParsedFile, function: Node<'_>, callable: &str, method: &str) -> Result<()> {
    let body = required_body(function, callable)?;
    if !calls_method(body, &file.source, method) {
        return unsupported(callable, &format!("implementation does not call {method}"));
    }
    Ok(())
}

fn require_named_call(
    file: &ParsedFile,
    function: Node<'_>,
    callable: &str,
    called: &str,
) -> Result<()> {
    let body = required_body(function, callable)?;
    if !calls_named(body, &file.source, called) {
        return unsupported(callable, &format!("implementation does not call {called}"));
    }
    Ok(())
}

fn unsupported<T>(callable: &str, reason: &str) -> Result<T> {
    Err(Error::UnsupportedImplementation {
        callable: callable.to_owned(),
        reason: reason.to_owned(),
    })
}

fn calls_method(node: Node<'_>, source: &[u8], method: &str) -> bool {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "field_expression" && field_text(node, "field", source) == Some(method) {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    false
}

fn calls_named(node: Node<'_>, source: &[u8], expected: &str) -> bool {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .is_some_and(|function| match function.kind() {
                    "identifier" => text(function, source) == Some(expected),
                    "scoped_identifier" => field_text(function, "name", source) == Some(expected),
                    "field_expression" => field_text(function, "field", source) == Some(expected),
                    _ => false,
                })
        {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    false
}

fn normalize_counts_helper(body: Node<'_>, source: &[u8]) -> Option<NormalizedBehavior> {
    let mut cursor = body.walk();
    let statements = body
        .named_children(&mut cursor)
        .filter(|node| !node.is_extra())
        .collect::<Vec<_>>();
    let [declaration, traversal, result] = statements.as_slice() else {
        return None;
    };
    if declaration.kind() != "let_declaration" || result.kind() != "identifier" {
        return None;
    }
    let accumulator = declaration.child_by_field_name("pattern")?;
    let initializer = declaration.child_by_field_name("value")?;
    if accumulator.kind() != "identifier"
        || text(accumulator, source) != text(*result, source)
        || !is_map_initializer(initializer, source)
        || !is_for_each_increment(*traversal, accumulator, source)
    {
        return None;
    }
    Some(counts_program())
}

fn normalize_sorted(body: Node<'_>, source: &[u8], kind: SortedKind) -> Option<NormalizedBehavior> {
    let mut cursor = body.walk();
    let statements = body
        .named_children(&mut cursor)
        .filter(|node| !node.is_extra())
        .collect::<Vec<_>>();
    let [declaration, sort_statement, result] = statements.as_slice() else {
        return None;
    };
    let values = declaration.child_by_field_name("pattern")?;
    let initializer = declaration.child_by_field_name("value")?;
    if declaration.kind() != "let_declaration"
        || values.kind() != "identifier"
        || !is_vec_from_iter(initializer, source)
        || !is_sort_call(*sort_statement, values, kind, source)
        || !is_into_iter_call(*result, values, source)
    {
        return None;
    }
    Some(sorted_program(kind))
}

fn is_vec_from_iter(node: Node<'_>, source: &[u8]) -> bool {
    let function = node.child_by_field_name("function");
    let arguments = node.child_by_field_name("arguments");
    node.kind() == "call_expression"
        && function.is_some_and(|function| {
            function.kind() == "scoped_identifier"
                && field_text(function, "name", source) == Some("from_iter")
                && function
                    .child_by_field_name("path")
                    .is_some_and(|path| text(path, source) == Some("Vec"))
        })
        && arguments.is_some_and(|arguments| {
            arguments.named_child_count() == 1
                && arguments
                    .named_child(0)
                    .is_some_and(|input| text(input, source) == Some("self"))
        })
}

fn is_sort_call(statement: Node<'_>, values: Node<'_>, kind: SortedKind, source: &[u8]) -> bool {
    let call = statement.named_child(0).unwrap_or(statement);
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return false;
    };
    let expected_argument = match kind {
        SortedKind::Stable | SortedKind::Unstable => None,
        SortedKind::StableBy | SortedKind::UnstableBy => Some("cmp"),
        SortedKind::StableByKey | SortedKind::UnstableByKey => Some("f"),
    };
    call.kind() == "call_expression"
        && function.kind() == "field_expression"
        && field_text(function, "field", source) == Some(kind.method())
        && function
            .child_by_field_name("value")
            .is_some_and(|receiver| text(receiver, source) == text(values, source))
        && match expected_argument {
            None => arguments.named_child_count() == 0,
            Some(expected) => {
                arguments.named_child_count() == 1
                    && arguments
                        .named_child(0)
                        .is_some_and(|argument| text(argument, source) == Some(expected))
            }
        }
}

fn is_into_iter_call(node: Node<'_>, values: Node<'_>, source: &[u8]) -> bool {
    let call = if node.kind() == "expression_statement" {
        let Some(call) = node.named_child(0) else {
            return false;
        };
        call
    } else {
        node
    };
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    call.kind() == "call_expression"
        && function.kind() == "field_expression"
        && field_text(function, "field", source) == Some("into_iter")
        && function
            .child_by_field_name("value")
            .is_some_and(|receiver| text(receiver, source) == text(values, source))
        && call
            .child_by_field_name("arguments")
            .is_some_and(|arguments| arguments.named_child_count() == 0)
}

fn is_map_initializer(initializer: Node<'_>, source: &[u8]) -> bool {
    if initializer.kind() != "call_expression" {
        return false;
    }
    let Some(function) = initializer.child_by_field_name("function") else {
        return false;
    };
    match function.kind() {
        "field_expression" => {
            matches!(
                field_text(function, "field", source),
                Some("new" | "with_hasher")
            ) && function
                .child_by_field_name("value")
                .is_some_and(|value| text(value, source) == Some("HashMap"))
        }
        "scoped_identifier" => {
            matches!(
                field_text(function, "name", source),
                Some("new" | "with_hasher")
            ) && function
                .child_by_field_name("path")
                .is_some_and(|path| is_hashmap_type(path, source))
        }
        _ => false,
    }
}

fn is_hashmap_type(node: Node<'_>, source: &[u8]) -> bool {
    text(node, source) == Some("HashMap")
        || (node.kind() == "generic_type" && field_text(node, "type", source) == Some("HashMap"))
}

fn normalize_group_map_helper(body: Node<'_>, source: &[u8]) -> Option<NormalizedBehavior> {
    let mut cursor = body.walk();
    let statements = body.named_children(&mut cursor).collect::<Vec<_>>();
    let [declaration, traversal, result] = statements.as_slice() else {
        return None;
    };
    let accumulator = declaration.child_by_field_name("pattern")?;
    let initializer = declaration.child_by_field_name("value")?;
    if declaration.kind() != "let_declaration"
        || accumulator.kind() != "identifier"
        || result.kind() != "identifier"
        || text(accumulator, source) != text(*result, source)
        || !is_map_initializer(initializer, source)
        || !is_for_each_push(*traversal, accumulator, source)
    {
        return None;
    }
    Some(group_map_program())
}

fn is_for_each_push(statement: Node<'_>, accumulator: Node<'_>, source: &[u8]) -> bool {
    (|| {
        let call = statement.named_child(0).unwrap_or(statement);
        let function = call.child_by_field_name("function")?;
        if call.kind() != "call_expression"
            || function.kind() != "field_expression"
            || field_text(function, "field", source) != Some("for_each")
        {
            return Some(false);
        }
        let arguments = call.child_by_field_name("arguments")?;
        let closure = arguments.named_child(0)?;
        let parameters = closure.child_by_field_name("parameters")?;
        let pair = parameters.named_child(0)?;
        if arguments.named_child_count() != 1
            || closure.kind() != "closure_expression"
            || pair.kind() != "tuple_pattern"
            || pair.named_child_count() != 2
        {
            return Some(false);
        }
        let key = pair.named_child(0)?;
        let value = pair.named_child(1)?;
        let closure_body = closure.child_by_field_name("body")?;
        let push_statement = if closure_body.kind() == "block" {
            if closure_body.named_child_count() != 1 {
                return Some(false);
            }
            closure_body.named_child(0)?
        } else {
            closure_body
        };
        Some(is_entry_push(
            push_statement,
            accumulator,
            key,
            value,
            source,
        ))
    })()
    .unwrap_or(false)
}

fn is_entry_push(
    statement: Node<'_>,
    accumulator: Node<'_>,
    key: Node<'_>,
    value: Node<'_>,
    source: &[u8],
) -> bool {
    let push_call = statement.named_child(0).unwrap_or(statement);
    let Some(push_field) = push_call.child_by_field_name("function") else {
        return false;
    };
    let Some(push_arguments) = push_call.child_by_field_name("arguments") else {
        return false;
    };
    let Some(or_default_call) = push_field.child_by_field_name("value") else {
        return false;
    };
    let Some(or_default_field) = or_default_call.child_by_field_name("function") else {
        return false;
    };
    let Some(entry_call) = or_default_field.child_by_field_name("value") else {
        return false;
    };
    let Some(entry_field) = entry_call.child_by_field_name("function") else {
        return false;
    };
    let Some(entry_arguments) = entry_call.child_by_field_name("arguments") else {
        return false;
    };
    push_call.kind() == "call_expression"
        && field_text(push_field, "field", source) == Some("push")
        && push_arguments.named_child_count() == 1
        && push_arguments
            .named_child(0)
            .is_some_and(|argument| text(argument, source) == text(value, source))
        && field_text(or_default_field, "field", source) == Some("or_default")
        && field_text(entry_field, "field", source) == Some("entry")
        && entry_field
            .child_by_field_name("value")
            .is_some_and(|map| text(map, source) == text(accumulator, source))
        && entry_arguments.named_child_count() == 1
        && entry_arguments
            .named_child(0)
            .is_some_and(|argument| text(argument, source) == text(key, source))
}

fn is_for_each_increment(statement: Node<'_>, accumulator: Node<'_>, source: &[u8]) -> bool {
    let call = statement.named_child(0).unwrap_or(statement);
    if call.kind() != "call_expression" {
        return false;
    }
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    if function.kind() != "field_expression"
        || field_text(function, "field", source) != Some("for_each")
        || function
            .child_by_field_name("value")
            .is_none_or(|input| text(input, source) != Some("self"))
    {
        return false;
    }
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return false;
    };
    let Some(closure) = arguments.named_child(0) else {
        return false;
    };
    if arguments.named_child_count() != 1 || closure.kind() != "closure_expression" {
        return false;
    }
    let Some(parameters) = closure.child_by_field_name("parameters") else {
        return false;
    };
    let Some(item) = parameters.named_child(0) else {
        return false;
    };
    let Some(assignment) = closure.child_by_field_name("body") else {
        return false;
    };
    parameters.named_child_count() == 1
        && item.kind() == "identifier"
        && is_entry_increment(assignment, accumulator, item, source)
}

fn is_entry_increment(
    assignment: Node<'_>,
    accumulator: Node<'_>,
    item: Node<'_>,
    source: &[u8],
) -> bool {
    if assignment.kind() != "compound_assignment_expr"
        || !has_child_kind(assignment, "+=")
        || assignment
            .child_by_field_name("right")
            .and_then(|right| text(right, source))
            != Some("1")
    {
        return false;
    }
    let Some(left) = assignment.child_by_field_name("left") else {
        return false;
    };
    let Some(or_default_call) = left.named_child(0) else {
        return false;
    };
    let Some(or_default_field) = or_default_call.child_by_field_name("function") else {
        return false;
    };
    let Some(entry_call) = or_default_field.child_by_field_name("value") else {
        return false;
    };
    let Some(entry_field) = entry_call.child_by_field_name("function") else {
        return false;
    };
    let Some(entry_arguments) = entry_call.child_by_field_name("arguments") else {
        return false;
    };
    left.kind() == "unary_expression"
        && has_child_kind(left, "*")
        && or_default_call.kind() == "call_expression"
        && field_text(or_default_field, "field", source) == Some("or_default")
        && entry_call.kind() == "call_expression"
        && field_text(entry_field, "field", source) == Some("entry")
        && entry_field
            .child_by_field_name("value")
            .is_some_and(|value| text(value, source) == text(accumulator, source))
        && entry_arguments.named_child_count() == 1
        && entry_arguments
            .named_child(0)
            .is_some_and(|key| text(key, source) == text(item, source))
}

fn evidence(
    file: &ParsedFile,
    function: Node<'_>,
    callable_path: &str,
) -> Result<ImplementationEvidence> {
    Ok(ImplementationEvidence {
        callable_path: callable_path.to_owned(),
        span: source_span(file, function)?,
        source_sha256: file.provenance.source_sha256.clone(),
    })
}

fn source_span(file: &ParsedFile, node: Node<'_>) -> Result<SourceSpan> {
    Ok(SourceSpan {
        path: file.path.clone(),
        start_byte: node
            .start_byte()
            .try_into()
            .map_err(|_| Error::SourceTooLarge {
                path: file.path.clone(),
            })?,
        end_byte: node
            .end_byte()
            .try_into()
            .map_err(|_| Error::SourceTooLarge {
                path: file.path.clone(),
            })?,
        start_line: (node.start_position().row + 1).try_into().map_err(|_| {
            Error::SourceTooLarge {
                path: file.path.clone(),
            }
        })?,
        end_line: (node.end_position().row + 1)
            .try_into()
            .map_err(|_| Error::SourceTooLarge {
                path: file.path.clone(),
            })?,
    })
}

fn has_child_kind(node: Node<'_>, expected: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == expected)
}

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a [u8]) -> Option<&'a str> {
    text(node.child_by_field_name(field)?, source)
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}
