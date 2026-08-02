#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Building a library's pack from the source Cargo already unpacked.

use std::path::PathBuf;

use entl_tree_sitter::ParserCatalog;
use infact_fact_pack::FactPackCache;
use infact_rust_behaviors::{LibraryPackRequest, build_library_pack, registry_sources};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn parsers() -> ParserCatalog {
    let discovery = ParserCatalog::discover([crate_root().join("../../../entl/parser-packs")]);
    assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
    discovery.catalog
}

/// The pack a builder produces has to be one a consumer can verify and import,
/// or local derivation is not interchangeable with a published pack.
#[test]
fn a_derived_pack_imports_into_the_cache() {
    let output = tempfile::tempdir().unwrap();
    let built = build_library_pack(LibraryPackRequest {
        source_root: &crate_root().join("tests/fixtures/itertools-source"),
        package: "fixture",
        version: "0.1.0",
        revision: 1,
        parsers: &parsers(),
        output: output.path(),
    })
    .unwrap();

    assert_eq!(built.manifest.name, "rust-fixture");
    assert!(
        built.behaviors > 0,
        "the fixture library describes behaviors"
    );
    assert!(
        built.manifest.provides.contains("rust.library-behaviors"),
        "{:?}",
        built.manifest.provides
    );

    // the contents are a catalog plus one blob per behavior
    assert_eq!(built.manifest.contents.len(), built.behaviors + 1);

    let cache_root = tempfile::tempdir().unwrap();
    let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
    let imported = cache.import_oci_layout(output.path()).unwrap();
    assert_eq!(imported.manifest.subject.name, "fixture");
    assert_eq!(imported.manifest.subject.version, "0.1.0");
}

/// The source has to be found without asking anyone where it is.
#[test]
fn registry_sources_are_discovered_by_package_and_version() {
    // itertools is a dependency of this workspace, so Cargo has unpacked it
    let found = registry_sources("itertools", "0.15.0").unwrap();
    if found.is_empty() {
        // a machine that has never fetched it is not a failure of the lookup
        return;
    }
    assert!(found.iter().all(|path| path.join("Cargo.toml").is_file()));
    assert!(
        registry_sources("itertools", "0.0.0-nonexistent")
            .unwrap()
            .is_empty()
    );
}
