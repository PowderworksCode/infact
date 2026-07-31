//! Fact-pack manifests independent of storage and registry transport.

mod cache;
mod lock;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const FACT_PACK_SCHEMA: u32 = 1;
pub const FACT_PACK_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.infact.fact-pack.v1";
pub const FACT_PACK_CONFIG_MEDIA_TYPE: &str = "application/vnd.infact.fact-pack.v1+toml";
const OCI_IMAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_IMAGE_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";

pub use cache::{CacheError, CachedContent, CachedFactPack, FactPackCache, FactPackRequirement};
pub use lock::{FACT_PACK_LOCK_SCHEMA, FactPackLock, FactPackLockError, LockedFactPack};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FactPackManifest {
    pub schema: u32,
    pub name: String,
    pub revision: u32,
    pub subject: Subject,
    pub sources: Vec<SourceInput>,
    pub derivation: Derivation,
    #[serde(default)]
    pub compatibility: Compatibility,
    pub provides: BTreeSet<String>,
    #[serde(default)]
    pub requires: BTreeSet<String>,
    pub contents: Vec<Content>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Subject {
    pub kind: SubjectKind,
    pub language: String,
    pub ecosystem: Option<String>,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SourceInput {
    pub kind: SourceKind,
    pub name: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Package,
    Repository,
    Toolchain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubjectKind {
    Language,
    Library,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Derivation {
    pub generator: String,
    pub generator_version: String,
    pub analyzer_sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Compatibility {
    pub compiler: Option<Compiler>,
    pub target: Option<Target>,
    #[serde(default)]
    pub package_features: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Compiler {
    pub name: String,
    pub version: String,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Target {
    pub triple: String,
    pub cpu: Option<String>,
    #[serde(default)]
    pub features: BTreeSet<String>,
    #[serde(default)]
    pub cfg: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Content {
    pub path: String,
    pub kind: String,
    pub media_type: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltLayout {
    pub manifest_digest: String,
    pub config_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciDescriptor {
    media_type: String,
    digest: String,
    size: usize,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    annotations: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciManifest {
    schema_version: u32,
    media_type: String,
    artifact_type: String,
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciIndex {
    schema_version: u32,
    media_type: String,
    manifests: Vec<OciDescriptor>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OciLayout {
    image_layout_version: String,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("parsing fact-pack TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serializing fact-pack TOML: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("invalid fact-pack manifest: {0}")]
    Invalid(String),
    #[error("reading fact-pack content {}: {source}", path.display())]
    ReadContent {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("fact-pack content {} escapes the pack root", path.display())]
    ContentEscapesRoot { path: std::path::PathBuf },
    #[error("fact-pack content {path} has digest {actual}, expected {expected}")]
    ContentDigest {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("serializing OCI metadata: {0}")]
    OciJson(#[from] serde_json::Error),
    #[error("OCI output directory {} is not empty", path.display())]
    OutputNotEmpty { path: PathBuf },
    #[error("{operation} {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("fact-pack source tree contains unsupported entry {}: {reason}", path.display())]
    UnsupportedTreeEntry { path: PathBuf, reason: &'static str },
}

impl FactPackManifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(source)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        let mut errors = Vec::new();
        if self.schema != FACT_PACK_SCHEMA {
            errors.push(format!(
                "schema is {}, supported schema is {FACT_PACK_SCHEMA}",
                self.schema
            ));
        }
        validate_slug("name", &self.name, &mut errors);
        if self.revision == 0 {
            errors.push("revision must be at least 1".to_owned());
        }
        self.subject.validate(&mut errors);
        if self.sources.is_empty() {
            errors.push("sources must contain at least one derivation input".to_owned());
        }
        let mut previous_source = None;
        for source in &self.sources {
            source.validate(&mut errors);
            let key = (source.kind, source.name.as_str(), source.version.as_str());
            if let Some(previous) = previous_source
                && previous >= key
            {
                errors.push("sources must be sorted by unique kind, name, and version".to_owned());
                break;
            }
            previous_source = Some(key);
        }
        self.derivation.validate(&mut errors);
        self.compatibility.validate(&mut errors);
        if self.provides.is_empty() {
            errors.push("provides must contain at least one fact capability".to_owned());
        }
        for capability in &self.provides {
            validate_capability("provides", capability, &mut errors);
        }
        for capability in &self.requires {
            validate_capability("requires", capability, &mut errors);
        }
        if self.contents.is_empty() {
            errors.push("contents must contain at least one blob".to_owned());
        }
        let mut previous = None;
        for content in &self.contents {
            content.validate(&mut errors);
            if let Some(previous) = previous
                && previous >= content.path.as_str()
            {
                errors.push("contents must be sorted by unique path".to_owned());
                break;
            }
            previous = Some(content.path.as_str());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ManifestError::Invalid(errors.join("; ")))
        }
    }

    pub fn to_canonical_toml(&self) -> Result<String, ManifestError> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn manifest_sha256(&self) -> Result<String, ManifestError> {
        let source = self.to_canonical_toml()?;
        Ok(sha256(source.as_bytes()))
    }

    pub fn verify_contents(&self, root: &Path) -> Result<(), ManifestError> {
        self.validate()?;
        let root = fs::canonicalize(root).map_err(|source| ManifestError::ReadContent {
            path: root.to_path_buf(),
            source,
        })?;
        for content in &self.contents {
            let path = root.join(&content.path);
            let canonical =
                fs::canonicalize(&path).map_err(|source| ManifestError::ReadContent {
                    path: path.clone(),
                    source,
                })?;
            if !canonical.starts_with(&root) {
                return Err(ManifestError::ContentEscapesRoot { path });
            }
            let bytes = fs::read(&canonical).map_err(|source| ManifestError::ReadContent {
                path: canonical,
                source,
            })?;
            let actual = sha256(&bytes);
            if actual != content.sha256 {
                return Err(ManifestError::ContentDigest {
                    path: content.path.clone(),
                    expected: content.sha256.clone(),
                    actual,
                });
            }
        }
        Ok(())
    }

    pub fn is_compatible_with(&self, context: &Compatibility) -> bool {
        &self.compatibility == context
    }
}

pub fn build_oci_layout(
    manifest: &FactPackManifest,
    pack_root: &Path,
    output: &Path,
) -> Result<BuiltLayout, ManifestError> {
    manifest.verify_contents(pack_root)?;
    prepare_output(output)?;
    let blob_root = output.join("blobs/sha256");
    create_dir_all(&blob_root)?;

    let config = manifest.to_canonical_toml()?.into_bytes();
    let config_descriptor = write_blob(
        &blob_root,
        FACT_PACK_CONFIG_MEDIA_TYPE,
        config,
        BTreeMap::new(),
    )?;
    let config_digest = config_descriptor.digest.clone();

    let canonical_root = canonicalize(pack_root)?;
    let mut layers = Vec::with_capacity(manifest.contents.len());
    for content in &manifest.contents {
        let bytes = read_content(&canonical_root, content)?;
        let actual = sha256(&bytes);
        if actual != content.sha256 {
            return Err(ManifestError::ContentDigest {
                path: content.path.clone(),
                expected: content.sha256.clone(),
                actual,
            });
        }
        let annotations = BTreeMap::from([(
            "org.opencontainers.image.title".to_owned(),
            content.path.clone(),
        )]);
        layers.push(write_blob(
            &blob_root,
            &content.media_type,
            bytes,
            annotations,
        )?);
    }

    let artifact_manifest = serde_json::to_vec(&OciManifest {
        schema_version: 2,
        media_type: OCI_IMAGE_MANIFEST_MEDIA_TYPE.to_owned(),
        artifact_type: FACT_PACK_ARTIFACT_MEDIA_TYPE.to_owned(),
        config: config_descriptor,
        layers,
    })?;
    let artifact_descriptor = write_blob(
        &blob_root,
        OCI_IMAGE_MANIFEST_MEDIA_TYPE,
        artifact_manifest,
        BTreeMap::from([(
            "org.opencontainers.image.ref.name".to_owned(),
            format!("{}-r{}", manifest.subject.version, manifest.revision),
        )]),
    )?;
    let manifest_digest = artifact_descriptor.digest.clone();

    write_file(
        &output.join("oci-layout"),
        &serde_json::to_vec(&OciLayout {
            image_layout_version: "1.0.0".to_owned(),
        })?,
    )?;
    write_file(
        &output.join("index.json"),
        &serde_json::to_vec(&OciIndex {
            schema_version: 2,
            media_type: OCI_IMAGE_INDEX_MEDIA_TYPE.to_owned(),
            manifests: vec![artifact_descriptor],
        })?,
    )?;

    Ok(BuiltLayout {
        manifest_digest,
        config_digest,
    })
}

impl Subject {
    fn validate(&self, errors: &mut Vec<String>) {
        validate_slug("subject.language", &self.language, errors);
        if let Some(ecosystem) = &self.ecosystem {
            validate_slug("subject.ecosystem", ecosystem, errors);
        } else if self.kind == SubjectKind::Library {
            errors.push("subject.ecosystem is required for a library".to_owned());
        }
        validate_slug("subject.name", &self.name, errors);
        validate_text("subject.version", &self.version, errors);
    }
}

impl SourceInput {
    fn validate(&self, errors: &mut Vec<String>) {
        validate_text("sources.name", &self.name, errors);
        validate_text("sources.version", &self.version, errors);
        validate_sha256("sources.sha256", &self.sha256, errors);
    }
}

impl Derivation {
    fn validate(&self, errors: &mut Vec<String>) {
        validate_slug("derivation.generator", &self.generator, errors);
        validate_text(
            "derivation.generator-version",
            &self.generator_version,
            errors,
        );
        validate_sha256("derivation.analyzer-sha256", &self.analyzer_sha256, errors);
    }
}

impl Compatibility {
    fn validate(&self, errors: &mut Vec<String>) {
        if let Some(compiler) = &self.compiler {
            validate_slug("compatibility.compiler.name", &compiler.name, errors);
            validate_text("compatibility.compiler.version", &compiler.version, errors);
            if let Some(commit) = &compiler.commit {
                validate_text("compatibility.compiler.commit", commit, errors);
            }
        }
        if let Some(target) = &self.target {
            validate_text("compatibility.target.triple", &target.triple, errors);
            if let Some(cpu) = &target.cpu {
                validate_text("compatibility.target.cpu", cpu, errors);
            }
            for feature in &target.features {
                validate_text("compatibility.target.features", feature, errors);
            }
            for cfg in &target.cfg {
                validate_text("compatibility.target.cfg", cfg, errors);
            }
        }
        for feature in &self.package_features {
            validate_text("compatibility.package-features", feature, errors);
        }
    }
}

impl Content {
    fn validate(&self, errors: &mut Vec<String>) {
        if !is_safe_relative_path(&self.path) {
            errors.push(format!(
                "contents.path `{}` must be a normalized relative path",
                self.path
            ));
        }
        validate_slug("contents.kind", &self.kind, errors);
        if !self.media_type.contains('/')
            || self
                .media_type
                .bytes()
                .any(|byte| byte.is_ascii_whitespace())
        {
            errors.push(format!(
                "contents.media-type `{}` is not a media type",
                self.media_type
            ));
        }
        validate_sha256("contents.sha256", &self.sha256, errors);
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub fn source_tree_sha256(root: &Path) -> Result<String, ManifestError> {
    let root = canonicalize(root)?;
    let mut files = Vec::new();
    collect_source_files(&root, &root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hash = Sha256::new();
    hash.update(b"infact-source-tree-v1\0");
    for (path, absolute) in files {
        let bytes = fs::read(&absolute).map_err(|source| ManifestError::Io {
            operation: "reading source tree file",
            path: absolute,
            source,
        })?;
        hash.update((path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn collect_source_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), ManifestError> {
    let entries = fs::read_dir(directory).map_err(|source| ManifestError::Io {
        operation: "reading source tree directory",
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ManifestError::Io {
            operation: "reading source tree entry",
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| ManifestError::Io {
            operation: "inspecting source tree entry",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ManifestError::UnsupportedTreeEntry {
                path,
                reason: "symbolic links are not included",
            });
        }
        if metadata.is_dir() {
            collect_source_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("collected entries remain below the source root");
            let components = relative
                .components()
                .map(|component| {
                    component.as_os_str().to_str().ok_or_else(|| {
                        ManifestError::UnsupportedTreeEntry {
                            path: path.clone(),
                            reason: "paths must be UTF-8",
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            files.push((components.join("/"), path));
        }
    }
    Ok(())
}

fn prepare_output(output: &Path) -> Result<(), ManifestError> {
    match fs::read_dir(output) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(ManifestError::OutputNotEmpty {
                    path: output.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_dir_all(output)?,
        Err(source) => {
            return Err(ManifestError::Io {
                operation: "inspecting OCI output directory",
                path: output.to_path_buf(),
                source,
            });
        }
    }
    Ok(())
}

fn create_dir_all(path: &Path) -> Result<(), ManifestError> {
    fs::create_dir_all(path).map_err(|source| ManifestError::Io {
        operation: "creating directory",
        path: path.to_path_buf(),
        source,
    })
}

fn canonicalize(path: &Path) -> Result<PathBuf, ManifestError> {
    fs::canonicalize(path).map_err(|source| ManifestError::ReadContent {
        path: path.to_path_buf(),
        source,
    })
}

fn read_content(root: &Path, content: &Content) -> Result<Vec<u8>, ManifestError> {
    let path = root.join(&content.path);
    let canonical = canonicalize(&path)?;
    if !canonical.starts_with(root) {
        return Err(ManifestError::ContentEscapesRoot { path });
    }
    fs::read(&canonical).map_err(|source| ManifestError::ReadContent {
        path: canonical,
        source,
    })
}

fn write_blob(
    blob_root: &Path,
    media_type: &str,
    bytes: Vec<u8>,
    annotations: BTreeMap<String, String>,
) -> Result<OciDescriptor, ManifestError> {
    let digest = sha256(&bytes);
    let path = blob_root.join(
        digest
            .strip_prefix("sha256:")
            .expect("sha256 always includes the algorithm prefix"),
    );
    if !path.exists() {
        write_file(&path, &bytes)?;
    }
    Ok(OciDescriptor {
        media_type: media_type.to_owned(),
        digest,
        size: bytes.len(),
        annotations,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ManifestError> {
    fs::write(path, bytes).map_err(|source| ManifestError::Io {
        operation: "writing OCI layout",
        path: path.to_path_buf(),
        source,
    })
}

fn validate_slug(field: &str, value: &str, errors: &mut Vec<String>) {
    let valid = !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        errors.push(format!(
            "{field} `{value}` must contain lowercase ASCII letters, digits, and internal hyphens"
        ));
    }
}

fn validate_capability(field: &str, value: &str, errors: &mut Vec<String>) {
    if value.split('.').any(|segment| {
        segment.is_empty()
            || segment.starts_with('-')
            || segment.ends_with('-')
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    }) {
        errors.push(format!(
            "{field} capability `{value}` must be dot-separated lowercase names"
        ));
    }
}

fn validate_text(field: &str, value: &str, errors: &mut Vec<String>) {
    if value.is_empty() || value.chars().any(char::is_control) {
        errors.push(format!("{field} must be nonempty text without controls"));
    }
}

fn validate_sha256(field: &str, value: &str, errors: &mut Vec<String>) {
    let Some(hex) = value.strip_prefix("sha256:") else {
        errors.push(format!("{field} must start with `sha256:`"));
        return;
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        errors.push(format!(
            "{field} must contain 64 lowercase hexadecimal digits"
        ));
    }
}

fn is_safe_relative_path(value: &str) -> bool {
    if value.is_empty() || value.contains('\\') || value.ends_with('/') {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    const ONE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn manifest() -> FactPackManifest {
        FactPackManifest {
            schema: FACT_PACK_SCHEMA,
            name: "rust-itertools".to_owned(),
            revision: 1,
            subject: Subject {
                kind: SubjectKind::Library,
                language: "rust".to_owned(),
                ecosystem: Some("cargo".to_owned()),
                name: "itertools".to_owned(),
                version: "0.15.0".to_owned(),
            },
            sources: vec![SourceInput {
                kind: SourceKind::Package,
                name: "itertools".to_owned(),
                version: "0.15.0".to_owned(),
                sha256: ZERO.to_owned(),
            }],
            derivation: Derivation {
                generator: "infact".to_owned(),
                generator_version: "0.0.0".to_owned(),
                analyzer_sha256: ONE.to_owned(),
            },
            compatibility: Compatibility {
                compiler: Some(Compiler {
                    name: "rustc".to_owned(),
                    version: "1.93.1".to_owned(),
                    commit: Some("abc123".to_owned()),
                }),
                target: Some(Target {
                    triple: "aarch64-apple-darwin".to_owned(),
                    cpu: Some("apple-m2".to_owned()),
                    features: BTreeSet::from(["neon".to_owned()]),
                    cfg: BTreeSet::from(["target-pointer-width=64".to_owned()]),
                }),
                package_features: BTreeSet::from(["use-std".to_owned()]),
            },
            provides: BTreeSet::from([
                "rust.call-effects".to_owned(),
                "rust.library-behaviors".to_owned(),
            ]),
            requires: BTreeSet::from(["rust.syntax-tree".to_owned()]),
            contents: vec![Content {
                path: "behaviors/counts.json".to_owned(),
                kind: "library-behavior".to_owned(),
                media_type: "application/vnd.infact.library-behavior.v1+json".to_owned(),
                sha256: ZERO.to_owned(),
            }],
        }
    }

    #[test]
    fn canonical_toml_round_trips_and_hashes_stably() {
        let manifest = manifest();
        let source = manifest.to_canonical_toml().unwrap();
        let parsed = FactPackManifest::parse(&source).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.manifest_sha256().unwrap(), sha256(source.as_bytes()));
    }

    #[test]
    fn compatibility_is_exact() {
        let manifest = manifest();
        assert!(manifest.is_compatible_with(&manifest.compatibility));
        let mut other = manifest.compatibility.clone();
        other.package_features.insert("serde".to_owned());
        assert!(!manifest.is_compatible_with(&other));
    }

    #[test]
    fn rejects_unsafe_and_unsorted_contents() {
        let mut manifest = manifest();
        manifest.contents = vec![
            Content {
                path: "types/z.json".to_owned(),
                kind: "type-facts".to_owned(),
                media_type: "application/vnd.infact.types.v1+json".to_owned(),
                sha256: ZERO.to_owned(),
            },
            Content {
                path: "../secret".to_owned(),
                kind: "type-facts".to_owned(),
                media_type: "application/vnd.infact.types.v1+json".to_owned(),
                sha256: ZERO.to_owned(),
            },
        ];
        let error = manifest.validate().unwrap_err().to_string();
        assert!(error.contains("normalized relative path"));
        assert!(error.contains("sorted by unique path"));
    }

    #[test]
    fn library_requires_an_ecosystem() {
        let mut manifest = manifest();
        manifest.subject.ecosystem = None;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("ecosystem is required")
        );
    }

    #[test]
    fn verifies_content_digests() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("behaviors")).unwrap();
        fs::write(directory.path().join("behaviors/counts.json"), b"{}\n").unwrap();
        let mut manifest = manifest();
        manifest.contents[0].sha256 = sha256(b"{}\n");
        manifest.verify_contents(directory.path()).unwrap();
        manifest.contents[0].sha256 = ZERO.to_owned();
        let error = manifest
            .verify_contents(directory.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("has digest"));
    }

    #[test]
    fn builds_a_deterministic_oci_layout() {
        let pack = tempfile::tempdir().unwrap();
        fs::create_dir_all(pack.path().join("behaviors")).unwrap();
        fs::write(pack.path().join("behaviors/counts.json"), b"{}\n").unwrap();
        let mut manifest = manifest();
        manifest.contents[0].sha256 = sha256(b"{}\n");

        let first = tempfile::tempdir().unwrap();
        let first_output = first.path().join("layout");
        let first_build = build_oci_layout(&manifest, pack.path(), &first_output).unwrap();
        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(first_output.join("index.json")).unwrap()).unwrap();
        assert_eq!(index["schemaVersion"], 2);
        assert_eq!(index["manifests"][0]["digest"], first_build.manifest_digest);
        assert_eq!(
            index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"],
            "0.15.0-r1"
        );

        let second = tempfile::tempdir().unwrap();
        let second_output = second.path().join("layout");
        let second_build = build_oci_layout(&manifest, pack.path(), &second_output).unwrap();
        assert_eq!(first_build, second_build);
        assert_eq!(
            fs::read(first_output.join("index.json")).unwrap(),
            fs::read(second_output.join("index.json")).unwrap()
        );

        let error = build_oci_layout(&manifest, pack.path(), &first_output)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not empty"));
    }

    #[test]
    fn source_tree_hash_uses_paths_and_contents() {
        let first = tempfile::tempdir().unwrap();
        fs::create_dir_all(first.path().join("src")).unwrap();
        fs::write(first.path().join("Cargo.toml"), b"[package]\n").unwrap();
        fs::write(first.path().join("src/lib.rs"), b"pub fn one() {}\n").unwrap();

        let second = tempfile::tempdir().unwrap();
        fs::create_dir_all(second.path().join("src")).unwrap();
        fs::write(second.path().join("src/lib.rs"), b"pub fn one() {}\n").unwrap();
        fs::write(second.path().join("Cargo.toml"), b"[package]\n").unwrap();
        assert_eq!(
            source_tree_sha256(first.path()).unwrap(),
            source_tree_sha256(second.path()).unwrap()
        );

        fs::write(second.path().join("src/lib.rs"), b"pub fn two() {}\n").unwrap();
        assert_ne!(
            source_tree_sha256(first.path()).unwrap(),
            source_tree_sha256(second.path()).unwrap()
        );
    }
}
