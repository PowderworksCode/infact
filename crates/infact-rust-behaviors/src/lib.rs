//! Facts matching Rust code behavior to external library APIs.

mod derivation;
mod macro_derivation;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use entl_tree_sitter::{ParsedFile, ParserCatalog, parse_repository};
use heck::{ToKebabCase, ToSnakeCase};
use infact_core::{
    CallableSignature, DERIVED_LIBRARY_BEHAVIOR_SCHEMA, DERIVED_MACRO_BEHAVIOR_SCHEMA, Derivation,
    DerivedLibraryBehavior, DerivedMacroBehavior, EXTERNAL_CATALOG_SCHEMA, ExternalBound,
    ExternalCallable, ExternalCatalog, ExternalType, Fact, InputEvidence, LibraryBehaviorMatch,
    LibraryBehaviorPattern, LibraryTarget, MacroBehavior, SourceSpan, StringCase,
};
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

pub use derivation::derive_behavior;
pub use macro_derivation::{MacroDerivationRequest, derive_macro_behavior};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub files_parsed: usize,
    pub matches: Vec<Fact<LibraryBehaviorMatch>>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Parser(#[from] entl_tree_sitter::Error),
    #[error("source file {path} is too large for source coordinates")]
    SourceTooLarge { path: PathBuf },
    #[error(
        "external catalog for {package} {version} uses schema {actual}; supported schema is {expected}"
    )]
    UnsupportedCatalogSchema {
        package: String,
        version: String,
        actual: u32,
        expected: u32,
    },
    #[error("automatic behavior derivation is not supported for {callable}")]
    UnsupportedDerivation { callable: String },
    #[error("external catalog does not contain {callable}")]
    MissingCallable { callable: String },
    #[error("external catalog contains an incompatible signature for {callable}")]
    IncompatibleCallable { callable: String },
    #[error("could not find the source implementation of {callable}")]
    MissingImplementation { callable: String },
    #[error("cannot normalize the implementation of {callable}: {reason}")]
    UnsupportedImplementation { callable: String, reason: String },
    #[error("derived behavior for {callable} uses schema {actual}; supported schema is {expected}")]
    UnsupportedBehaviorSchema {
        callable: String,
        actual: u32,
        expected: u32,
    },
    #[error(
        "derived macro behavior for {derive} uses schema {actual}; supported schema is {expected}"
    )]
    UnsupportedMacroBehaviorSchema {
        derive: String,
        actual: u32,
        expected: u32,
    },
    #[error("no Rust parser pack is configured")]
    MissingRustParser,
    #[error("{derive} produced Rust containing parse errors")]
    InvalidMacroExpansion { derive: String },
    #[error("probe for {derive} contains Rust parse errors")]
    InvalidMacroProbe { derive: String },
    #[error("cannot normalize the expansion of {derive}: {reason}")]
    UnsupportedMacroExpansion { derive: String, reason: String },
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn analyze_repository(
    root: impl AsRef<Path>,
    parsers: &ParserCatalog,
    catalogs: &[ExternalCatalog],
    behaviors: &[DerivedLibraryBehavior],
    macro_behaviors: &[DerivedMacroBehavior],
) -> Result<AnalysisReport> {
    for catalog in catalogs {
        if catalog.schema != EXTERNAL_CATALOG_SCHEMA {
            return Err(Error::UnsupportedCatalogSchema {
                package: catalog.package.clone(),
                version: catalog.version.clone(),
                actual: catalog.schema,
                expected: EXTERNAL_CATALOG_SCHEMA,
            });
        }
    }
    for behavior in behaviors {
        if behavior.schema != DERIVED_LIBRARY_BEHAVIOR_SCHEMA {
            return Err(Error::UnsupportedBehaviorSchema {
                callable: behavior.callable_path.clone(),
                actual: behavior.schema,
                expected: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
            });
        }
    }
    for behavior in macro_behaviors {
        if behavior.schema != DERIVED_MACRO_BEHAVIOR_SCHEMA {
            return Err(Error::UnsupportedMacroBehaviorSchema {
                derive: behavior.derive_path.clone(),
                actual: behavior.schema,
                expected: DERIVED_MACRO_BEHAVIOR_SCHEMA,
            });
        }
    }
    let parsed = parse_repository(root, parsers)?;
    let join = catalogs.iter().find_map(itertools_join);
    let counts = catalogs
        .iter()
        .find_map(|catalog| itertools_counts(catalog, behaviors));
    let counts_by = catalogs
        .iter()
        .find_map(|catalog| itertools_counts_by(catalog, behaviors));
    let group_map = catalogs
        .iter()
        .find_map(|catalog| itertools_group_map(catalog, behaviors));
    let group_map_by = catalogs
        .iter()
        .find_map(|catalog| itertools_group_map_by(catalog, behaviors));
    let sorted = derivation::SortedKind::ALL
        .into_iter()
        .filter_map(|kind| {
            catalogs
                .iter()
                .find_map(|catalog| itertools_sorted(catalog, behaviors, kind))
                .map(|(catalog, callable)| (kind, catalog, callable))
        })
        .collect::<Vec<_>>();
    let mut matches = BTreeSet::new();

    for file in &parsed.files {
        if file.pack.language().id != "rust" {
            continue;
        }
        if let Some((catalog, callable)) = join {
            collect_join_matches(file, catalog, callable, &mut matches)?;
        }
        if let Some((catalog, callable)) = counts {
            collect_counts_matches(file, catalog, callable, &mut matches)?;
        }
        if let Some((catalog, callable)) = counts_by {
            collect_counts_by_matches(file, catalog, callable, &mut matches)?;
        }
        if let Some((catalog, callable)) = group_map {
            collect_group_map_matches(file, catalog, callable, false, &mut matches)?;
        }
        if let Some((catalog, callable)) = group_map_by {
            collect_group_map_matches(file, catalog, callable, true, &mut matches)?;
        }
        for &(kind, catalog, callable) in &sorted {
            collect_sorted_matches(file, catalog, callable, kind, &mut matches)?;
        }
        collect_enum_macro_matches(file, macro_behaviors, &mut matches)?;
    }

    Ok(AnalysisReport {
        files_parsed: parsed.files.len(),
        matches: matches.into_iter().collect(),
        diagnostics: parsed
            .diagnostics
            .into_iter()
            .map(|diagnostic| AnalysisDiagnostic {
                path: diagnostic.path,
                message: diagnostic.message,
            })
            .collect(),
    })
}

fn itertools_sorted<'a>(
    catalog: &'a ExternalCatalog,
    behaviors: &[DerivedLibraryBehavior],
    kind: derivation::SortedKind,
) -> Option<(&'a ExternalCatalog, &'a ExternalCallable)> {
    derived_callable(
        catalog,
        behaviors,
        kind.path(),
        |signature| is_sorted_signature(signature, kind),
        derivation::sorted_program(kind),
    )
}

fn itertools_counts<'a>(
    catalog: &'a ExternalCatalog,
    behaviors: &[DerivedLibraryBehavior],
) -> Option<(&'a ExternalCatalog, &'a ExternalCallable)> {
    derived_callable(
        catalog,
        behaviors,
        "itertools::Itertools::counts",
        is_counts_signature,
        derivation::counts_program(),
    )
}

fn itertools_counts_by<'a>(
    catalog: &'a ExternalCatalog,
    behaviors: &[DerivedLibraryBehavior],
) -> Option<(&'a ExternalCatalog, &'a ExternalCallable)> {
    derived_callable(
        catalog,
        behaviors,
        "itertools::Itertools::counts_by",
        is_counts_by_signature,
        derivation::counts_by_program(),
    )
}

fn itertools_group_map<'a>(
    catalog: &'a ExternalCatalog,
    behaviors: &[DerivedLibraryBehavior],
) -> Option<(&'a ExternalCatalog, &'a ExternalCallable)> {
    derived_callable(
        catalog,
        behaviors,
        "itertools::Itertools::into_group_map",
        is_group_map_signature,
        derivation::group_map_program(),
    )
}

fn itertools_group_map_by<'a>(
    catalog: &'a ExternalCatalog,
    behaviors: &[DerivedLibraryBehavior],
) -> Option<(&'a ExternalCatalog, &'a ExternalCallable)> {
    derived_callable(
        catalog,
        behaviors,
        "itertools::Itertools::into_group_map_by",
        is_group_map_by_signature,
        derivation::group_map_by_program(),
    )
}

fn derived_callable<'a>(
    catalog: &'a ExternalCatalog,
    behaviors: &[DerivedLibraryBehavior],
    path: &str,
    compatible: impl Fn(&CallableSignature) -> bool,
    program: infact_core::NormalizedBehavior,
) -> Option<(&'a ExternalCatalog, &'a ExternalCallable)> {
    if catalog.package != "itertools" {
        return None;
    }
    let callable = catalog
        .callables
        .iter()
        .find(|callable| callable.path == path && compatible(&callable.signature))?;
    behaviors
        .iter()
        .any(|behavior| {
            behavior.callable_package == catalog.package
                && behavior.callable_version == catalog.version
                && behavior.callable_path == callable.path
                && behavior.catalog_sha256 == catalog.source_sha256
                && behavior.program == program
        })
        .then_some((catalog, callable))
}

fn is_counts_signature(signature: &CallableSignature) -> bool {
    let input_matches = matches!(
        signature.inputs.as_slice(),
        [parameter] if matches!(parameter.ty, ExternalType::Generic { ref name } if name == "Self")
    );
    let output_matches = matches!(
        signature.output,
        Some(ExternalType::Path {
            ref path,
            ref arguments,
        }) if path == "HashMap"
            && arguments.len() == 2
            && is_self_item(&arguments[0])
            && matches!(arguments[1], ExternalType::Primitive { ref name } if name == "usize")
    );
    let item_requirements = signature
        .requirements
        .iter()
        .find(|requirement| is_self_item(&requirement.subject));
    let has_bound = |expected: &str| {
        item_requirements.is_some_and(|requirement| {
            requirement.bounds.iter().any(|bound| {
                matches!(bound, ExternalBound::Trait { path } if path_name(path) == expected)
            })
        })
    };
    input_matches && output_matches && has_bound("Eq") && has_bound("Hash")
}

pub(crate) fn is_counts_by_signature(signature: &CallableSignature) -> bool {
    matches!(
        signature.inputs.as_slice(),
        [receiver, function]
            if is_generic(&receiver.ty, "Self") && is_generic(&function.ty, "F")
    ) && is_hashmap_output(signature, |key, value| {
        is_generic(key, "K") && matches!(value, ExternalType::Primitive { name } if name == "usize")
    }) && has_generic_bounds(signature, "K", &["Eq", "Hash"])
        && has_generic_bounds(signature, "F", &["FnMut"])
}

pub(crate) fn is_group_map_signature(signature: &CallableSignature) -> bool {
    matches!(
        signature.inputs.as_slice(),
        [receiver] if is_generic(&receiver.ty, "Self")
    ) && is_group_map_output(signature)
        && has_generic_bounds(signature, "K", &["Eq", "Hash"])
}

pub(crate) fn is_group_map_by_signature(signature: &CallableSignature) -> bool {
    matches!(
        signature.inputs.as_slice(),
        [receiver, function]
            if is_generic(&receiver.ty, "Self") && is_generic(&function.ty, "F")
    ) && is_group_map_output(signature)
        && has_generic_bounds(signature, "K", &["Eq", "Hash"])
        && has_generic_bounds(signature, "F", &["FnMut"])
}

pub(crate) fn is_sorted_signature(
    signature: &CallableSignature,
    kind: derivation::SortedKind,
) -> bool {
    let inputs_match = match kind {
        derivation::SortedKind::Stable | derivation::SortedKind::Unstable => {
            matches!(signature.inputs.as_slice(), [receiver] if is_generic(&receiver.ty, "Self"))
        }
        _ => matches!(
            signature.inputs.as_slice(),
            [receiver, function]
                if is_generic(&receiver.ty, "Self") && is_generic(&function.ty, "F")
        ),
    };
    let output_matches = matches!(
        signature.output,
        Some(ExternalType::Path {
            ref path,
            ref arguments,
        }) if path_name(path) == "IntoIter"
            && matches!(arguments.as_slice(), [item] if is_self_item(item))
    );
    let requirements_match = match kind {
        derivation::SortedKind::Stable | derivation::SortedKind::Unstable => {
            has_type_bounds(signature, is_self_item, &["Ord"])
        }
        derivation::SortedKind::StableBy | derivation::SortedKind::UnstableBy => {
            has_generic_bounds(signature, "F", &["FnMut"])
        }
        derivation::SortedKind::StableByKey | derivation::SortedKind::UnstableByKey => {
            has_generic_bounds(signature, "F", &["FnMut"])
                && has_generic_bounds(signature, "K", &["Ord"])
        }
    };
    inputs_match && output_matches && requirements_match
}

fn is_group_map_output(signature: &CallableSignature) -> bool {
    is_hashmap_output(signature, |key, value| {
        is_generic(key, "K")
            && matches!(
                value,
                ExternalType::Path { path, arguments }
                    if path == "Vec"
                        && matches!(arguments.as_slice(), [value] if is_generic(value, "V"))
            )
    })
}

fn is_hashmap_output(
    signature: &CallableSignature,
    arguments_match: impl FnOnce(&ExternalType, &ExternalType) -> bool,
) -> bool {
    matches!(
        signature.output,
        Some(ExternalType::Path {
            ref path,
            ref arguments,
        }) if path == "HashMap"
            && matches!(arguments.as_slice(), [key, value] if arguments_match(key, value))
    )
}

fn is_generic(ty: &ExternalType, expected: &str) -> bool {
    matches!(ty, ExternalType::Generic { name } if name == expected)
}

fn has_generic_bounds(signature: &CallableSignature, generic: &str, expected: &[&str]) -> bool {
    let Some(requirement) = signature
        .requirements
        .iter()
        .find(|requirement| is_generic(&requirement.subject, generic))
    else {
        return false;
    };
    expected.iter().all(|expected| {
        requirement.bounds.iter().any(
            |bound| matches!(bound, ExternalBound::Trait { path } if path_name(path) == *expected),
        )
    })
}

fn has_type_bounds(
    signature: &CallableSignature,
    subject_matches: impl Fn(&ExternalType) -> bool,
    expected: &[&str],
) -> bool {
    signature
        .requirements
        .iter()
        .find(|requirement| subject_matches(&requirement.subject))
        .is_some_and(|requirement| {
            expected.iter().all(|expected| {
                requirement.bounds.iter().any(
                    |bound| matches!(bound, ExternalBound::Trait { path } if path_name(path) == *expected),
                )
            })
        })
}

fn is_self_item(ty: &ExternalType) -> bool {
    matches!(
        ty,
        ExternalType::Associated {
            name,
            self_type,
            ..
        } if name == "Item"
            && matches!(self_type.as_ref(), ExternalType::Generic { name } if name == "Self")
    )
}

fn path_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

fn itertools_join(catalog: &ExternalCatalog) -> Option<(&ExternalCatalog, &ExternalCallable)> {
    if catalog.package != "itertools" {
        return None;
    }
    catalog
        .callables
        .iter()
        .find(|callable| {
            callable.path == "itertools::Itertools::join"
                && is_string_join_signature(&callable.signature)
        })
        .map(|callable| (catalog, callable))
}

fn is_string_join_signature(signature: &CallableSignature) -> bool {
    let output_is_string = matches!(
        signature.output,
        Some(ExternalType::Path { ref path, .. }) if path == "String"
    );
    let separator_is_str = signature.inputs.last().is_some_and(|parameter| {
        matches!(
            parameter.ty,
            ExternalType::Reference {
                inner: ref ty,
                ..
            } if matches!(ty.as_ref(), ExternalType::Primitive { name } if name == "str")
        )
    });
    let item_is_display = signature.requirements.iter().any(|requirement| {
        requirement.bounds.iter().any(
            |bound| matches!(bound, ExternalBound::Trait { path } if path_name(path) == "Display"),
        )
    });
    output_is_string && separator_is_str && item_is_display
}

fn collect_join_matches(
    file: &ParsedFile,
    catalog: &ExternalCatalog,
    callable: &ExternalCallable,
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_collect_vec_join(node, &file.source) {
            output.insert(behavior_match(
                file,
                node,
                node,
                catalog,
                callable,
                LibraryBehaviorPattern::IteratorCollectVecJoin,
            )?);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn collect_counts_matches(
    file: &ParsedFile,
    catalog: &ExternalCatalog,
    callable: &ExternalCallable,
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "block" {
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            for statements in children.windows(3) {
                if manual_count_projection(
                    statements[0],
                    statements[1],
                    statements[2],
                    &file.source,
                ) == Some(false)
                {
                    output.insert(behavior_match(
                        file,
                        statements[0],
                        statements[2],
                        catalog,
                        callable,
                        LibraryBehaviorPattern::IteratorManualCounts,
                    )?);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn collect_counts_by_matches(
    file: &ParsedFile,
    catalog: &ExternalCatalog,
    callable: &ExternalCallable,
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "block" {
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            for statements in children.windows(3) {
                if manual_count_projection(
                    statements[0],
                    statements[1],
                    statements[2],
                    &file.source,
                ) == Some(true)
                {
                    output.insert(behavior_match(
                        file,
                        statements[0],
                        statements[2],
                        catalog,
                        callable,
                        LibraryBehaviorPattern::IteratorManualCountsBy,
                    )?);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn manual_count_projection(
    declaration: Node<'_>,
    loop_statement: Node<'_>,
    result: Node<'_>,
    source: &[u8],
) -> Option<bool> {
    if declaration.kind() != "let_declaration" || result.kind() != "identifier" {
        return None;
    }
    let accumulator = declaration.child_by_field_name("pattern")?;
    let initializer = declaration.child_by_field_name("value")?;
    if accumulator.kind() != "identifier"
        || node_text(accumulator, source) != node_text(result, source)
        || !is_hashmap_usize_new(initializer, source)
    {
        return None;
    }
    let for_expression = loop_statement.named_child(0)?;
    if loop_statement.kind() != "expression_statement" || for_expression.kind() != "for_expression"
    {
        return None;
    }
    let item = for_expression.child_by_field_name("pattern")?;
    let body = for_expression.child_by_field_name("body")?;
    if item.kind() != "identifier" || body.named_child_count() != 1 {
        return None;
    }
    let key = counts_increment_key(body.named_child(0)?, accumulator, source)?;
    if node_text(key, source) == node_text(item, source) {
        Some(false)
    } else {
        contains_identifier(key, item, source).then_some(true)
    }
}

fn is_hashmap_usize_new(initializer: Node<'_>, source: &[u8]) -> bool {
    if initializer.kind() != "call_expression" {
        return false;
    }
    let Some(function) = initializer.child_by_field_name("function") else {
        return false;
    };
    let Some(arguments) = initializer.child_by_field_name("arguments") else {
        return false;
    };
    if function.kind() != "scoped_identifier"
        || field_text(function, "name", source) != Some("new")
        || arguments.named_child_count() != 0
    {
        return false;
    }
    let Some(map_type) = function.child_by_field_name("path") else {
        return false;
    };
    if map_type.kind() != "generic_type" || field_text(map_type, "type", source) != Some("HashMap")
    {
        return false;
    }
    let Some(arguments) = map_type.child_by_field_name("type_arguments") else {
        return false;
    };
    arguments.named_child_count() == 2
        && arguments
            .named_child(1)
            .is_some_and(|count| node_text(count, source) == Some("usize"))
}

fn counts_increment_key<'tree>(
    statement: Node<'tree>,
    accumulator: Node<'tree>,
    source: &[u8],
) -> Option<Node<'tree>> {
    let assignment = statement.named_child(0)?;
    if statement.kind() != "expression_statement"
        || assignment.kind() != "compound_assignment_expr"
        || !has_child_kind(assignment, "+=")
        || assignment
            .child_by_field_name("right")
            .and_then(|right| node_text(right, source))
            != Some("1")
    {
        return None;
    }
    let left = assignment.child_by_field_name("left")?;
    if left.kind() != "unary_expression" || !has_child_kind(left, "*") {
        return None;
    }
    let or_default_call = left.named_child(0)?;
    let or_default_field = or_default_call.child_by_field_name("function")?;
    if or_default_call.kind() != "call_expression"
        || field_name(or_default_field, source) != Some("or_default")
        || or_default_call
            .child_by_field_name("arguments")
            .is_none_or(|arguments| arguments.named_child_count() != 0)
    {
        return None;
    }
    let entry_call = or_default_field.child_by_field_name("value")?;
    let entry_field = entry_call.child_by_field_name("function")?;
    let entry_arguments = entry_call.child_by_field_name("arguments")?;
    (entry_call.kind() == "call_expression"
        && field_name(entry_field, source) == Some("entry")
        && entry_field
            .child_by_field_name("value")
            .is_some_and(|value| node_text(value, source) == node_text(accumulator, source))
        && entry_arguments.named_child_count() == 1)
        .then(|| entry_arguments.named_child(0))
        .flatten()
}

fn contains_identifier(node: Node<'_>, identifier: Node<'_>, source: &[u8]) -> bool {
    let expected = node_text(identifier, source);
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && node_text(node, source) == expected {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    false
}

fn collect_group_map_matches(
    file: &ParsedFile,
    catalog: &ExternalCatalog,
    callable: &ExternalCallable,
    projected: bool,
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "block" {
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            for statements in children.windows(3) {
                if manual_group_projection(
                    statements[0],
                    statements[1],
                    statements[2],
                    &file.source,
                ) == Some(projected)
                {
                    output.insert(behavior_match(
                        file,
                        statements[0],
                        statements[2],
                        catalog,
                        callable,
                        if projected {
                            LibraryBehaviorPattern::IteratorManualGroupMapBy
                        } else {
                            LibraryBehaviorPattern::IteratorManualGroupMap
                        },
                    )?);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn manual_group_projection(
    declaration: Node<'_>,
    loop_statement: Node<'_>,
    result: Node<'_>,
    source: &[u8],
) -> Option<bool> {
    if declaration.kind() != "let_declaration" || result.kind() != "identifier" {
        return None;
    }
    let accumulator = declaration.child_by_field_name("pattern")?;
    let initializer = declaration.child_by_field_name("value")?;
    if accumulator.kind() != "identifier"
        || node_text(accumulator, source) != node_text(result, source)
        || !is_hashmap_vec_new(initializer, source)
    {
        return None;
    }
    let for_expression = loop_statement.named_child(0)?;
    if loop_statement.kind() != "expression_statement" || for_expression.kind() != "for_expression"
    {
        return None;
    }
    let pattern = for_expression.child_by_field_name("pattern")?;
    let body = for_expression.child_by_field_name("body")?;
    if body.named_child_count() != 1 {
        return None;
    }
    let (key, value) = entry_push_parts(body.named_child(0)?, accumulator, source)?;
    if pattern.kind() == "tuple_pattern" && pattern.named_child_count() == 2 {
        let expected_key = pattern.named_child(0)?;
        let expected_value = pattern.named_child(1)?;
        return (node_text(key, source) == node_text(expected_key, source)
            && node_text(value, source) == node_text(expected_value, source))
        .then_some(false);
    }
    if pattern.kind() == "identifier"
        && node_text(value, source) == node_text(pattern, source)
        && contains_identifier(key, pattern, source)
    {
        return Some(true);
    }
    None
}

fn is_hashmap_vec_new(initializer: Node<'_>, source: &[u8]) -> bool {
    let Some(function) = initializer.child_by_field_name("function") else {
        return false;
    };
    if initializer.kind() != "call_expression"
        || function.kind() != "scoped_identifier"
        || field_text(function, "name", source) != Some("new")
        || initializer
            .child_by_field_name("arguments")
            .is_none_or(|arguments| arguments.named_child_count() != 0)
    {
        return false;
    }
    let Some(map_type) = function.child_by_field_name("path") else {
        return false;
    };
    let Some(arguments) = map_type.child_by_field_name("type_arguments") else {
        return false;
    };
    map_type.kind() == "generic_type"
        && field_text(map_type, "type", source) == Some("HashMap")
        && arguments.named_child_count() == 2
        && arguments.named_child(1).is_some_and(|value| {
            value.kind() == "generic_type" && field_text(value, "type", source) == Some("Vec")
        })
}

fn entry_push_parts<'tree>(
    statement: Node<'tree>,
    accumulator: Node<'tree>,
    source: &[u8],
) -> Option<(Node<'tree>, Node<'tree>)> {
    let push_call = statement.named_child(0)?;
    let push_field = push_call.child_by_field_name("function")?;
    let push_arguments = push_call.child_by_field_name("arguments")?;
    let or_default_call = push_field.child_by_field_name("value")?;
    let or_default_field = or_default_call.child_by_field_name("function")?;
    let entry_call = or_default_field.child_by_field_name("value")?;
    let entry_field = entry_call.child_by_field_name("function")?;
    let entry_arguments = entry_call.child_by_field_name("arguments")?;
    if statement.kind() != "expression_statement"
        || push_call.kind() != "call_expression"
        || field_name(push_field, source) != Some("push")
        || push_arguments.named_child_count() != 1
        || field_name(or_default_field, source) != Some("or_default")
        || field_name(entry_field, source) != Some("entry")
        || entry_field
            .child_by_field_name("value")
            .is_none_or(|map| node_text(map, source) != node_text(accumulator, source))
        || entry_arguments.named_child_count() != 1
    {
        return None;
    }
    Some((
        entry_arguments.named_child(0)?,
        push_arguments.named_child(0)?,
    ))
}

fn has_child_kind(node: Node<'_>, expected: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == expected)
}

fn field_text<'a>(node: Node<'_>, field: &str, source: &'a [u8]) -> Option<&'a str> {
    node_text(node.child_by_field_name(field)?, source)
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn collect_sorted_matches(
    file: &ParsedFile,
    catalog: &ExternalCatalog,
    callable: &ExternalCallable,
    kind: derivation::SortedKind,
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "block" {
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            for statements in children.windows(2) {
                if is_collect_then_sort(statements[0], statements[1], kind, &file.source) {
                    output.insert(behavior_match(
                        file,
                        statements[0],
                        statements[1],
                        catalog,
                        callable,
                        sorted_pattern(kind),
                    )?);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn sorted_pattern(kind: derivation::SortedKind) -> LibraryBehaviorPattern {
    match kind {
        derivation::SortedKind::Stable => LibraryBehaviorPattern::IteratorCollectThenSort,
        derivation::SortedKind::StableBy => LibraryBehaviorPattern::IteratorCollectThenSortBy,
        derivation::SortedKind::StableByKey => LibraryBehaviorPattern::IteratorCollectThenSortByKey,
        derivation::SortedKind::Unstable => LibraryBehaviorPattern::IteratorCollectThenSortUnstable,
        derivation::SortedKind::UnstableBy => {
            LibraryBehaviorPattern::IteratorCollectThenSortUnstableBy
        }
        derivation::SortedKind::UnstableByKey => {
            LibraryBehaviorPattern::IteratorCollectThenSortUnstableByKey
        }
    }
}

fn is_collect_then_sort(
    declaration: Node<'_>,
    sort_statement: Node<'_>,
    kind: derivation::SortedKind,
    source: &[u8],
) -> bool {
    if declaration.kind() != "let_declaration" {
        return false;
    }
    let Some(values) = declaration.child_by_field_name("pattern") else {
        return false;
    };
    let Some(initializer) = declaration.child_by_field_name("value") else {
        return false;
    };
    values.kind() == "identifier"
        && is_vec_collect(declaration, initializer, source)
        && is_local_sort_call(sort_statement, values, kind, source)
}

fn is_vec_collect(declaration: Node<'_>, initializer: Node<'_>, source: &[u8]) -> bool {
    if initializer.kind() != "call_expression" {
        return false;
    }
    let Some(function) = initializer.child_by_field_name("function") else {
        return false;
    };
    let typed_declaration = declaration
        .child_by_field_name("type")
        .is_some_and(|ty| type_name(ty, source) == Some("Vec"));
    match function.kind() {
        "field_expression" => typed_declaration && field_name(function, source) == Some("collect"),
        "generic_function" => {
            let Some(collect) = function.child_by_field_name("function") else {
                return false;
            };
            let Some(arguments) = function.child_by_field_name("type_arguments") else {
                return false;
            };
            collect.kind() == "field_expression"
                && field_name(collect, source) == Some("collect")
                && is_vec_type_argument(arguments, source)
        }
        _ => false,
    }
}

fn type_name<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        "type_identifier" => node_text(node, source),
        "generic_type" => field_text(node, "type", source),
        _ => None,
    }
}

fn is_local_sort_call(
    statement: Node<'_>,
    values: Node<'_>,
    kind: derivation::SortedKind,
    source: &[u8],
) -> bool {
    let Some(call) = statement.named_child(0) else {
        return false;
    };
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return false;
    };
    let expected_arguments = match kind {
        derivation::SortedKind::Stable | derivation::SortedKind::Unstable => 0,
        _ => 1,
    };
    statement.kind() == "expression_statement"
        && call.kind() == "call_expression"
        && function.kind() == "field_expression"
        && field_name(function, source) == Some(kind.method())
        && function
            .child_by_field_name("value")
            .is_some_and(|receiver| node_text(receiver, source) == node_text(values, source))
        && arguments.named_child_count() == expected_arguments
}

fn collect_enum_macro_matches(
    file: &ParsedFile,
    macro_behaviors: &[DerivedMacroBehavior],
    output: &mut BTreeSet<Fact<LibraryBehaviorMatch>>,
) -> Result<()> {
    let display_behaviors = macro_behaviors
        .iter()
        .filter(|behavior| {
            behavior.macro_package == "strum"
                && behavior.derive_path == "strum::Display"
                && matches!(behavior.behavior, MacroBehavior::EnumDisplay { .. })
        })
        .collect::<Vec<_>>();
    let as_ref_behaviors = macro_behaviors
        .iter()
        .filter(|behavior| {
            behavior.macro_package == "strum"
                && behavior.derive_path == "strum::AsRefStr"
                && matches!(behavior.behavior, MacroBehavior::EnumAsRefStr { .. })
        })
        .collect::<Vec<_>>();
    let variant_array_behaviors = macro_behaviors
        .iter()
        .filter(|behavior| {
            behavior.macro_package == "strum"
                && behavior.derive_path == "strum::VariantArray"
                && behavior.behavior == MacroBehavior::EnumVariantArray
        })
        .collect::<Vec<_>>();
    if display_behaviors.is_empty()
        && as_ref_behaviors.is_empty()
        && variant_array_behaviors.is_empty()
    {
        return Ok(());
    }

    let mut stack = vec![file.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_item"
            && let Some((enum_name, variants)) = unit_enum(node, &file.source)
            && let Some(scope) = node.parent()
        {
            let mut cursor = scope.walk();
            let siblings = scope.named_children(&mut cursor).collect::<Vec<_>>();
            let inherent = siblings.iter().copied().find_map(|item| {
                (is_impl_for(item, None, &enum_name, &file.source))
                    .then(|| manual_as_str(item, &file.source))
                    .flatten()
                    .map(|mappings| (item, mappings))
            });
            let display = siblings.iter().copied().find(|item| {
                is_impl_for(*item, Some("Display"), &enum_name, &file.source)
                    && display_delegates_to_as_str(*item, &file.source)
            });
            if let (Some((_inherent, mappings)), Some(display)) = (&inherent, display)
                && mapping_is_exhaustive(mappings, &variants)
            {
                for behavior in &display_behaviors {
                    let MacroBehavior::EnumDisplay { case } = behavior.behavior else {
                        continue;
                    };
                    if mappings_match_case(mappings, case) {
                        output.insert(macro_behavior_match(
                            file,
                            node,
                            display,
                            behavior,
                            LibraryBehaviorPattern::EnumManualDisplay,
                        )?);
                    }
                }
            }
            if let Some((inherent, mappings)) = &inherent
                && mapping_is_exhaustive(mappings, &variants)
            {
                let preferred_case = enum_serde_case(node, &file.source);
                let matching = as_ref_behaviors
                    .iter()
                    .copied()
                    .filter(|behavior| {
                        let MacroBehavior::EnumAsRefStr { case } = behavior.behavior else {
                            return false;
                        };
                        preferred_case.is_none_or(|preferred| preferred == case)
                            && mappings_match_case(mappings, case)
                    })
                    .collect::<Vec<_>>();
                if preferred_case.is_some() || matching.len() == 1 {
                    for behavior in matching {
                        output.insert(macro_behavior_match(
                            file,
                            node,
                            *inherent,
                            behavior,
                            LibraryBehaviorPattern::EnumManualAsRefStr,
                        )?);
                    }
                }
            }
            if let Some(array_impl) = siblings.iter().copied().find(|item| {
                is_impl_for(*item, None, &enum_name, &file.source)
                    && manual_variant_array(*item, &variants, &file.source)
            }) {
                for behavior in &variant_array_behaviors {
                    output.insert(macro_behavior_match(
                        file,
                        node,
                        array_impl,
                        behavior,
                        LibraryBehaviorPattern::EnumManualVariantArray,
                    )?);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    Ok(())
}

fn enum_serde_case(node: Node<'_>, source: &[u8]) -> Option<StringCase> {
    let mut sibling = node.prev_named_sibling();
    while let Some(attribute) = sibling {
        if attribute.kind() != "attribute_item" {
            break;
        }
        let text = node_text(attribute, source)?;
        if text.contains("serde") && text.contains("rename_all") {
            if text.contains("kebab-case") {
                return Some(StringCase::Kebab);
            }
            if text.contains("snake_case") {
                return Some(StringCase::Snake);
            }
        }
        sibling = attribute.prev_named_sibling();
    }
    None
}

fn unit_enum(node: Node<'_>, source: &[u8]) -> Option<(String, Vec<String>)> {
    let name = field_text(node, "name", source)?.to_owned();
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let variants = body
        .named_children(&mut cursor)
        .map(|variant| {
            (variant.kind() == "enum_variant" && variant.named_child_count() == 1)
                .then(|| field_text(variant, "name", source).map(str::to_owned))
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    (!variants.is_empty()).then_some((name, variants))
}

fn mapping_is_exhaustive(mappings: &BTreeMap<String, String>, variants: &[String]) -> bool {
    mappings.keys().cloned().collect::<BTreeSet<_>>()
        == variants.iter().cloned().collect::<BTreeSet<_>>()
}

fn manual_variant_array(node: Node<'_>, variants: &[String], source: &[u8]) -> bool {
    let Some(body) = node.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor).any(|item| {
        if item.kind() != "const_item" {
            return false;
        }
        let Some(value) = item.child_by_field_name("value") else {
            return false;
        };
        let Some(array) = named_descendant(value, "array_expression") else {
            return false;
        };
        let mut cursor = array.walk();
        array
            .named_children(&mut cursor)
            .filter_map(|element| last_named_identifier(element, source).map(str::to_owned))
            .collect::<Vec<_>>()
            == variants
    })
}

fn named_descendant<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn is_impl_for(
    node: Node<'_>,
    expected_trait: Option<&str>,
    expected_type: &str,
    source: &[u8],
) -> bool {
    if node.kind() != "impl_item" || field_text(node, "type", source) != Some(expected_type) {
        return false;
    }
    match (node.child_by_field_name("trait"), expected_trait) {
        (None, None) => true,
        (Some(trait_node), Some(expected)) => {
            last_named_identifier(trait_node, source) == Some(expected)
        }
        _ => false,
    }
}

fn last_named_identifier<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    if matches!(node.kind(), "identifier" | "type_identifier") {
        return node_text(node, source);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|child| last_named_identifier(child, source))
        .last()
}

fn manual_as_str(node: Node<'_>, source: &[u8]) -> Option<BTreeMap<String, String>> {
    let function = impl_function(node, "as_str", source)?;
    let body = function.child_by_field_name("body")?;
    let match_expression = only_expression(body)?;
    if match_expression.kind() != "match_expression"
        || match_expression
            .child_by_field_name("value")
            .is_none_or(|value| value.kind() != "self")
    {
        return None;
    }
    let match_body = match_expression.child_by_field_name("body")?;
    let mut cursor = match_body.walk();
    match_body
        .named_children(&mut cursor)
        .map(|arm| {
            let pattern = arm.child_by_field_name("pattern")?;
            let value = arm.child_by_field_name("value")?;
            if arm.kind() != "match_arm" || value.kind() != "string_literal" {
                return None;
            }
            let variant = last_named_identifier(pattern, source)?.to_owned();
            let string = value.named_child(0).and_then(|content| {
                (content.kind() == "string_content")
                    .then(|| node_text(content, source))
                    .flatten()
            })?;
            Some((variant, string.to_owned()))
        })
        .collect()
}

fn impl_function<'tree>(node: Node<'tree>, expected: &str, source: &[u8]) -> Option<Node<'tree>> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    body.named_children(&mut cursor).find(|item| {
        item.kind() == "function_item" && field_text(*item, "name", source) == Some(expected)
    })
}

fn only_expression(block: Node<'_>) -> Option<Node<'_>> {
    if block.named_child_count() != 1 {
        return None;
    }
    let expression = block.named_child(0)?;
    (expression.kind() == "expression_statement")
        .then(|| expression.named_child(0))
        .flatten()
        .or(Some(expression))
}

fn display_delegates_to_as_str(node: Node<'_>, source: &[u8]) -> bool {
    let Some(function) = impl_function(node, "fmt", source) else {
        return false;
    };
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    let Some(call) = only_expression(body) else {
        return false;
    };
    let Some(fmt_field) = call.child_by_field_name("function") else {
        return false;
    };
    let Some(as_str_call) = fmt_field.child_by_field_name("value") else {
        return false;
    };
    let Some(as_str_field) = as_str_call.child_by_field_name("function") else {
        return false;
    };
    call.kind() == "call_expression"
        && fmt_field.kind() == "field_expression"
        && field_name(fmt_field, source) == Some("fmt")
        && as_str_call.kind() == "call_expression"
        && as_str_call
            .child_by_field_name("arguments")
            .is_some_and(|arguments| arguments.named_child_count() == 0)
        && as_str_field.kind() == "field_expression"
        && field_name(as_str_field, source) == Some("as_str")
        && as_str_field
            .child_by_field_name("value")
            .is_some_and(|value| value.kind() == "self")
}

fn mappings_match_case(mappings: &BTreeMap<String, String>, case: StringCase) -> bool {
    mappings.iter().all(|(variant, value)| match case {
        StringCase::Kebab => variant.to_kebab_case() == *value,
        StringCase::Snake => variant.to_snake_case() == *value,
    })
}

fn is_collect_vec_join(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(join_field) = node.child_by_field_name("function") else {
        return false;
    };
    if join_field.kind() != "field_expression" || field_name(join_field, source) != Some("join") {
        return false;
    }
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return false;
    };
    if arguments.named_child_count() != 1
        || !arguments.named_child(0).is_some_and(|argument| {
            matches!(argument.kind(), "string_literal" | "raw_string_literal")
        })
    {
        return false;
    }
    let Some(collect_call) = join_field.child_by_field_name("value") else {
        return false;
    };
    if collect_call.kind() != "call_expression" {
        return false;
    }
    let Some(collect_function) = collect_call.child_by_field_name("function") else {
        return false;
    };
    if collect_function.kind() != "generic_function" {
        return false;
    }
    let Some(collect_field) = collect_function.child_by_field_name("function") else {
        return false;
    };
    let Some(type_arguments) = collect_function.child_by_field_name("type_arguments") else {
        return false;
    };
    collect_field.kind() == "field_expression"
        && field_name(collect_field, source) == Some("collect")
        && is_vec_type_argument(type_arguments, source)
}

fn field_name<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let field = node.child_by_field_name("field")?;
    std::str::from_utf8(&source[field.byte_range()]).ok()
}

fn is_vec_type_argument(type_arguments: Node<'_>, source: &[u8]) -> bool {
    if type_arguments.named_child_count() != 1 {
        return false;
    }
    let Some(generic_type) = type_arguments.named_child(0) else {
        return false;
    };
    if generic_type.kind() != "generic_type" {
        return false;
    }
    generic_type
        .child_by_field_name("type")
        .is_some_and(|ty| source.get(ty.byte_range()) == Some(b"Vec"))
}

fn behavior_match(
    file: &ParsedFile,
    start: Node<'_>,
    end: Node<'_>,
    catalog: &ExternalCatalog,
    callable: &ExternalCallable,
    pattern: LibraryBehaviorPattern,
) -> Result<Fact<LibraryBehaviorMatch>> {
    let start_byte = u64::try_from(start.start_byte()).map_err(|_| Error::SourceTooLarge {
        path: file.path.clone(),
    })?;
    let end_byte = u64::try_from(end.end_byte()).map_err(|_| Error::SourceTooLarge {
        path: file.path.clone(),
    })?;
    let start_line =
        u32::try_from(start.start_position().row + 1).map_err(|_| Error::SourceTooLarge {
            path: file.path.clone(),
        })?;
    let end_line =
        u32::try_from(end.end_position().row + 1).map_err(|_| Error::SourceTooLarge {
            path: file.path.clone(),
        })?;
    Ok(Fact {
        value: LibraryBehaviorMatch {
            target: LibraryTarget::Callable {
                package: catalog.package.clone(),
                version: catalog.version.clone(),
                path: callable.path.clone(),
                catalog_sha256: catalog.source_sha256.clone(),
            },
            pattern,
            span: SourceSpan {
                path: file.path.clone(),
                start_byte,
                end_byte,
                start_line,
                end_line,
            },
        },
        derivation: Derivation {
            analyzer: "rust.library-behaviors".to_owned(),
            analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
            inputs: vec![InputEvidence {
                path: file.path.clone(),
                content_sha256: file.provenance.source_sha256.clone(),
                parser_id: file.provenance.parser_id.clone(),
                parser_version: file.provenance.parser_version.clone(),
                grammar_sha256: file.provenance.grammar_sha256.clone(),
            }],
        },
    })
}

fn macro_behavior_match(
    file: &ParsedFile,
    first: Node<'_>,
    second: Node<'_>,
    behavior: &DerivedMacroBehavior,
    pattern: LibraryBehaviorPattern,
) -> Result<Fact<LibraryBehaviorMatch>> {
    let (start, end) = if first.start_byte() <= second.start_byte() {
        (first, second)
    } else {
        (second, first)
    };
    let start_byte = u64::try_from(start.start_byte()).map_err(|_| Error::SourceTooLarge {
        path: file.path.clone(),
    })?;
    let end_byte = u64::try_from(end.end_byte()).map_err(|_| Error::SourceTooLarge {
        path: file.path.clone(),
    })?;
    let start_line =
        u32::try_from(start.start_position().row + 1).map_err(|_| Error::SourceTooLarge {
            path: file.path.clone(),
        })?;
    let end_line =
        u32::try_from(end.end_position().row + 1).map_err(|_| Error::SourceTooLarge {
            path: file.path.clone(),
        })?;
    Ok(Fact {
        value: LibraryBehaviorMatch {
            target: LibraryTarget::DeriveMacro {
                package: behavior.macro_package.clone(),
                version: behavior.macro_version.clone(),
                path: behavior.derive_path.clone(),
                expansion_sha256: behavior.expansion_sha256.clone(),
            },
            pattern,
            span: SourceSpan {
                path: file.path.clone(),
                start_byte,
                end_byte,
                start_line,
                end_line,
            },
        },
        derivation: Derivation {
            analyzer: "rust.library-behaviors".to_owned(),
            analyzer_version: env!("CARGO_PKG_VERSION").to_owned(),
            inputs: vec![InputEvidence {
                path: file.path.clone(),
                content_sha256: file.provenance.source_sha256.clone(),
                parser_id: file.provenance.parser_id.clone(),
                parser_version: file.provenance.parser_version.clone(),
                grammar_sha256: file.provenance.grammar_sha256.clone(),
            }],
        },
    })
}
