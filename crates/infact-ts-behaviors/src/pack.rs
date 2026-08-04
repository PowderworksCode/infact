//! Builds a JavaScript or TypeScript library's Infact pack from source on disk.
//!
//! An npm dependency's source is sitting in `node_modules` the moment it is
//! installed, and derivation needs nothing more than that: no build, no bundler,
//! no type checker. The standard library is the one that is not a package, and
//! it is why `ecosystem` is optional here — a pack derived from an engine's
//! self-hosted builtins describes the LANGUAGE, and says so in its subject.

use std::collections::BTreeSet;
use std::path::Path;

use entl_tree_sitter::ParserCatalog;
use infact_fact_pack::{
    BuiltLayout, Compatibility, Content, Derivation, FACT_PACK_SCHEMA, FactPackManifest,
    SourceInput, SourceKind, Subject, SubjectKind, build_oci_layout, sha256,
};

use crate::{Error, Result, derive_library};

/// What to build a pack for.
pub struct LibraryPackRequest<'a> {
    pub source_root: &'a Path,
    pub package: &'a str,
    pub version: &'a str,
    pub revision: u32,
    /// The package registry this came from, when it came from one.
    ///
    /// `None` says the subject is the language itself rather than a library on
    /// top of it, which is what a pack derived from an engine's self-hosted
    /// builtins describes. Recording that as `Some("npm")` would claim the
    /// standard library is an npm package.
    pub ecosystem: Option<&'a str>,
    pub parsers: &'a ParserCatalog,
    /// Where to write the OCI image layout.
    pub output: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltLibraryPack {
    pub manifest: FactPackManifest,
    pub layout: BuiltLayout,
    pub callables: usize,
    pub behaviors: usize,
}

/// A file name that identifies a behavior uniquely.
///
/// The leaf name alone does not: two containers routinely spell one method the
/// same way, and naming files after the leaf made one silently overwrite the
/// other — 23% of one pack, which is invisible unless the file count is
/// compared against what was derived.
pub fn behavior_file_name(package: &str, callable_path: &str, version: &str) -> String {
    let qualified = callable_path
        .strip_prefix(package)
        .unwrap_or(callable_path)
        .trim_start_matches(':');
    let slug = qualified.replace("::", "-").replace('_', "-").replace(
        |character: char| !character.is_ascii_alphanumeric() && character != '-',
        "-",
    );
    format!("{package}-{slug}-{version}.json")
}

/// Derive a library's behaviors and package them as an OCI image layout.
pub fn build_library_pack(request: LibraryPackRequest<'_>) -> Result<BuiltLibraryPack> {
    let derived = derive_library(
        request.source_root,
        request.parsers,
        request.package,
        request.version,
    )?;
    let (catalog, behaviors) = (derived.catalog, derived.behaviors);

    let staging = tempfile::tempdir().map_err(|source| Error::WritePack {
        path: std::path::PathBuf::from("<temporary>"),
        source,
    })?;
    let mut contents = Vec::new();

    let catalog_path = format!("api/{}-{}.json", request.package, request.version);
    let catalog_json = encode(&catalog)?;
    write_content(staging.path(), &catalog_path, &catalog_json)?;
    contents.push(Content {
        path: catalog_path,
        kind: "callable-signatures".to_owned(),
        media_type: "application/vnd.infact.external-catalog.v1+json".to_owned(),
        sha256: sha256(&catalog_json),
    });

    for behavior in &behaviors {
        let path = format!(
            "behaviors/{}",
            behavior_file_name(request.package, &behavior.callable_path, request.version)
        );
        let json = encode(behavior)?;
        write_content(staging.path(), &path, &json)?;
        contents.push(Content {
            path,
            kind: "library-behavior".to_owned(),
            media_type: "application/vnd.infact.library-behavior.v1+json".to_owned(),
            sha256: sha256(&json),
        });
    }
    contents.sort_by(|left, right| left.path.cmp(&right.path));

    let manifest = FactPackManifest {
        schema: FACT_PACK_SCHEMA,
        name: format!("typescript-{}", request.package),
        revision: request.revision,
        subject: Subject {
            kind: match request.ecosystem {
                Some(_) => SubjectKind::Library,
                None => SubjectKind::Language,
            },
            // The pack's language is the one its behaviors are matched against.
            // They are derived from JavaScript and matched into TypeScript,
            // which is the whole reason one normalizer serves both.
            language: "typescript".to_owned(),
            ecosystem: request.ecosystem.map(str::to_owned),
            name: request.package.to_owned(),
            version: request.version.to_owned(),
        },
        sources: vec![SourceInput {
            kind: match request.ecosystem {
                Some(_) => SourceKind::Package,
                None => SourceKind::Repository,
            },
            name: request.package.to_owned(),
            version: request.version.to_owned(),
            // the catalog's digest covers every source file it was read from
            sha256: catalog.source_sha256.clone(),
        }],
        derivation: Derivation {
            generator: "infact".to_owned(),
            generator_version: env!("CARGO_PKG_VERSION").to_owned(),
            analyzer_sha256: analyzer_sha256(),
        },
        compatibility: Compatibility::default(),
        provides: BTreeSet::from([
            "typescript.callable-signatures".to_owned(),
            "typescript.library-behaviors".to_owned(),
        ]),
        requires: BTreeSet::from(["typescript.syntax-tree".to_owned()]),
        contents,
    };

    let manifest_path = staging.path().join("pack.toml");
    std::fs::write(
        &manifest_path,
        manifest
            .to_canonical_toml()
            .map_err(|source| Error::PackManifest { source })?,
    )
    .map_err(|source| Error::WritePack {
        path: manifest_path,
        source,
    })?;

    let layout = build_oci_layout(&manifest, staging.path(), request.output)
        .map_err(|source| Error::PackManifest { source })?;
    Ok(BuiltLibraryPack {
        manifest,
        layout,
        callables: catalog.callables.len(),
        behaviors: behaviors.len(),
    })
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut json = serde_json::to_vec_pretty(value).map_err(Error::Encode)?;
    json.push(b'\n');
    Ok(json)
}

fn write_content(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::WritePack {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, bytes).map_err(|source| Error::WritePack { path, source })
}

/// A digest of the analyzer, so a pack records what produced it.
fn analyzer_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"infact-ts-behaviors-analyzer-v1\0");
    hasher.update(include_bytes!("derivation.rs"));
    hasher.update(include_bytes!("lib.rs"));
    format!("sha256:{:x}", hasher.finalize())
}
