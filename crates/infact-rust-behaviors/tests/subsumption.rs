#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Which of two behaviors that both match is the one worth reporting.

use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_core::{
    CallableContainer, DERIVED_LIBRARY_BEHAVIOR_SCHEMA, DerivedLibraryBehavior,
    EXTERNAL_CATALOG_SCHEMA, ExternalCallable, ExternalCatalog, Form, ImplementationEvidence,
    SourceSpan,
};
use infact_normalize::{Arm, Pattern};
use infact_rust_behaviors::analyze_repository;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `match self { Some(x) => taken, None => None }`, which is the shape every
/// way of consuming an `Option` shares.
fn consuming(taken: Form) -> Form {
    Form::select(
        Form::Free(0),
        vec![
            Arm {
                pattern: Pattern::Variant {
                    name: "Some".to_owned(),
                    parts: vec![Pattern::Binding(0)],
                },
                body: taken,
            },
            Arm {
                pattern: Pattern::Variant {
                    name: "None".to_owned(),
                    parts: Vec::new(),
                },
                body: Form::Variant {
                    name: "None".to_owned(),
                    payload: Vec::new(),
                },
            },
        ],
    )
}

fn applied() -> Form {
    Form::Call {
        callee: Box::new(Form::Free(1)),
        arguments: vec![Form::Local(0)],
    }
}

fn behavior(path: &str, program: Form) -> DerivedLibraryBehavior {
    DerivedLibraryBehavior {
        schema: DERIVED_LIBRARY_BEHAVIOR_SCHEMA,
        callable_package: "core".to_owned(),
        callable_version: "1.0.0".to_owned(),
        callable_path: path.to_owned(),
        catalog_sha256: "sha256:test".to_owned(),
        implementation: vec![ImplementationEvidence {
            callable_path: path.to_owned(),
            span: SourceSpan {
                path: PathBuf::from("src/option.rs"),
                start_byte: Some(0),
                end_byte: Some(1),
                start_line: 1,
                end_line: 1,
                start_column: None,
                end_column: None,
            },
            source_sha256: "sha256:elsewhere".to_owned(),
        }],
        program,
    }
}

fn catalog(paths: &[&str]) -> ExternalCatalog {
    ExternalCatalog {
        schema: EXTERNAL_CATALOG_SCHEMA,
        package: "core".to_owned(),
        version: "1.0.0".to_owned(),
        rustdoc_format: 1,
        source_sha256: "sha256:test".to_owned(),
        callables: paths
            .iter()
            .map(|path| ExternalCallable {
                path: (*path).to_owned(),
                container: CallableContainer::Type {
                    path: "Option".to_owned(),
                },
                signature: None,
            })
            .collect(),
    }
}

/// The broader of two behaviors stands aside where the narrower one landed.
///
/// `and_then` hands back whatever the caller's function returned, so its hole
/// swallows the `Some(..)` that `map` puts there. Both match this code; only
/// one of them says what it does.
#[test]
fn the_narrower_behavior_is_the_one_reported() {
    let parsers = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(parsers.errors.is_empty(), "{:?}", parsers.errors);
    let behaviors = vec![
        behavior(
            "core::Option::map",
            consuming(Form::Variant {
                name: "Some".to_owned(),
                payload: vec![applied()],
            }),
        ),
        behavior("core::Option::and_then", consuming(applied())),
    ];
    let catalogs = vec![catalog(&["core::Option::map", "core::Option::and_then"])];

    let report = analyze_repository(
        crate_root().join("tests/fixtures/subsumption"),
        &parsers.catalog,
        &catalogs,
        &behaviors,
        &[],
    )
    .unwrap();

    let reported: Vec<&str> = report
        .matches
        .iter()
        .map(|fact| fact.value.target.path())
        .collect();
    assert_eq!(
        reported,
        vec!["core::Option::map"],
        "and_then matches this too, and saying so as well is saying the weaker thing twice"
    );
}

/// Standing aside is per placement, not per pack.
///
/// A behavior that is broader than another is still the only thing that
/// describes code the narrower one does not reach, and dropping it from the
/// pack would lose that.
#[test]
fn a_broader_behavior_still_reports_where_nothing_narrower_matched() {
    let parsers = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    let behaviors = vec![behavior("core::Option::and_then", consuming(applied()))];
    let catalogs = vec![catalog(&["core::Option::and_then"])];

    let report = analyze_repository(
        crate_root().join("tests/fixtures/subsumption"),
        &parsers.catalog,
        &catalogs,
        &behaviors,
        &[],
    )
    .unwrap();

    assert_eq!(
        report
            .matches
            .iter()
            .map(|fact| fact.value.target.path())
            .collect::<Vec<_>>(),
        vec!["core::Option::and_then"]
    );
}
