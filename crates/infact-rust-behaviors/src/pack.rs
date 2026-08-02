//! Builds a library's Infact pack from the source already on disk.
//!
//! A dependency's source is sitting in the local registry the moment Cargo
//! resolves it, and derivation needs nothing more than that: no rustdoc, no
//! nightly, no successful build. So a pack for any dependency can be produced
//! on demand, which is what makes describing a whole dependency graph
//! practical rather than something someone has to publish first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

/// Where Cargo keeps the unpacked source of a registry dependency.
///
/// Returns every candidate, because a machine can have several registries
/// configured and the same package may be unpacked under more than one. An
/// empty result means the package is not unpacked; a registry that cannot be
/// read is a different answer and is returned as one, because otherwise a
/// broken checkout is indistinguishable from a missing dependency.
pub fn registry_sources(package: &str, version: &str) -> Result<Vec<PathBuf>> {
    let Some(home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".cargo")))
    else {
        return Ok(Vec::new());
    };
    let root = home.join("registry/src");
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = std::fs::read_dir(&root)
        .map_err(|source| Error::ReadRegistry {
            path: root.clone(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path().join(format!("{package}-{version}")))
                .map_err(|source| Error::ReadRegistry {
                    path: root.clone(),
                    source,
                })
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    found.sort();
    Ok(found)
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
        path: PathBuf::from("<temporary>"),
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
        name: format!("rust-{}", request.package),
        revision: request.revision,
        subject: Subject {
            kind: SubjectKind::Library,
            language: "rust".to_owned(),
            ecosystem: Some("cargo".to_owned()),
            name: request.package.to_owned(),
            version: request.version.to_owned(),
        },
        sources: vec![SourceInput {
            kind: SourceKind::Package,
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
            "rust.callable-signatures".to_owned(),
            "rust.library-behaviors".to_owned(),
        ]),
        requires: BTreeSet::from(["rust.syntax-tree".to_owned()]),
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

/// A file name that identifies a behavior uniquely.
///
/// The leaf name alone does not: `Option::unwrap_or` and `Result::unwrap_or`
/// are different behaviors with the same last segment, and naming files after
/// the leaf made one silently overwrite the other. Qualifying by the whole path
/// is what keeps a pack's file count equal to what was derived.
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
    hasher.update(b"infact-rust-behaviors-analyzer-v1\0");
    hasher.update(include_bytes!("derivation.rs"));
    hasher.update(include_bytes!("lib.rs"));
    format!("sha256:{:x}", hasher.finalize())
}
