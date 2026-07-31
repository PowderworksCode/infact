use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use super::{
    Compatibility, Derivation, FACT_PACK_ARTIFACT_MEDIA_TYPE, FACT_PACK_CONFIG_MEDIA_TYPE,
    FactPackManifest, ManifestError, OCI_IMAGE_INDEX_MEDIA_TYPE, OCI_IMAGE_MANIFEST_MEDIA_TYPE,
    OciDescriptor, OciIndex, OciLayout, OciManifest, SourceInput, Subject, sha256,
};

const CACHE_SCHEMA: u32 = 1;
const CACHE_MARKER: &str = "cache.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FactPackRequirement {
    pub name: String,
    pub subject: Subject,
    pub sources: Vec<SourceInput>,
    pub derivation: Derivation,
    pub compatibility: Compatibility,
    pub capabilities: BTreeSet<String>,
}

impl FactPackRequirement {
    pub fn from_manifest(manifest: &FactPackManifest) -> Self {
        Self {
            name: manifest.name.clone(),
            subject: manifest.subject.clone(),
            sources: manifest.sources.clone(),
            derivation: manifest.derivation.clone(),
            compatibility: manifest.compatibility.clone(),
            capabilities: manifest.provides.clone(),
        }
    }

    pub fn matches(&self, manifest: &FactPackManifest) -> bool {
        self.name == manifest.name
            && self.subject == manifest.subject
            && self.sources == manifest.sources
            && self.derivation == manifest.derivation
            && manifest.is_compatible_with(&self.compatibility)
            && self.capabilities.is_subset(&manifest.provides)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedContent {
    pub path: String,
    pub kind: String,
    pub media_type: String,
    pub digest: String,
    pub size: usize,
    pub blob_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFactPack {
    pub manifest: FactPackManifest,
    pub manifest_digest: String,
    pub config_digest: String,
    pub contents: Vec<CachedContent>,
}

#[derive(Debug, Clone)]
pub struct FactPackCache {
    root: PathBuf,
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("{operation} {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parsing OCI JSON {}: {source}", path.display())]
    OciJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("parsing fact-pack cache metadata {}: {source}", path.display())]
    CacheToml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing fact-pack cache metadata: {0}")]
    SerializeCache(#[from] toml::ser::Error),
    #[error("invalid OCI fact-pack layout: {0}")]
    InvalidLayout(String),
    #[error("invalid fact-pack cache: {0}")]
    InvalidCache(String),
    #[error("fact-pack blob {digest} has size {actual}, expected {expected}")]
    BlobSize {
        digest: String,
        expected: usize,
        actual: usize,
    },
    #[error("fact-pack blob has digest {actual}, expected {expected}")]
    BlobDigest { expected: String, actual: String },
    #[error("fact-pack {digest} is not installed in the local cache")]
    NotInstalled { digest: String },
    #[error("multiple revision {revision} packs satisfy {name}")]
    Ambiguous { name: String, revision: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CacheMarker {
    schema: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CacheEntry {
    schema: u32,
    manifest_digest: String,
}

struct DecodedPack {
    pack: CachedFactPack,
    blobs: BTreeMap<String, Vec<u8>>,
}

impl FactPackCache {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CacheError> {
        let root = root.as_ref().to_path_buf();
        prepare_cache_root(&root)?;
        create_dir_all(&root.join("blobs/sha256"))?;
        create_dir_all(&root.join("entries/sha256"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn import_oci_layout(
        &self,
        layout: impl AsRef<Path>,
    ) -> Result<CachedFactPack, CacheError> {
        let decoded = decode_layout(layout.as_ref())?;
        for (digest, bytes) in &decoded.blobs {
            self.store_blob(digest, bytes)?;
        }
        self.store_entry(&decoded.pack.manifest_digest)?;
        self.load(&decoded.pack.manifest_digest)
    }

    /// Import a registry artifact after its manifest and referenced blobs have
    /// been downloaded. The entry becomes visible only after the complete
    /// artifact validates against the same rules as a local OCI layout.
    pub fn import_oci_artifact(
        &self,
        manifest_digest: &str,
        manifest_bytes: &[u8],
        blobs: &BTreeMap<String, Vec<u8>>,
    ) -> Result<CachedFactPack, CacheError> {
        verify_digest(manifest_digest, manifest_bytes)?;
        for (digest, bytes) in blobs {
            self.store_blob(digest, bytes)?;
        }
        self.store_blob(manifest_digest, manifest_bytes)?;
        let pack = decode_cached_pack(&self.root.join("blobs/sha256"), manifest_digest)?;
        self.store_entry(&pack.manifest_digest)?;
        self.load(&pack.manifest_digest)
    }

    pub fn load(&self, manifest_digest: &str) -> Result<CachedFactPack, CacheError> {
        validate_digest(manifest_digest)?;
        let entry = self.entry_path(manifest_digest);
        if !entry.is_file() {
            return Err(CacheError::NotInstalled {
                digest: manifest_digest.to_owned(),
            });
        }
        let source = read(&entry, "reading fact-pack cache entry")?;
        let record: CacheEntry = parse_toml(&entry, &source)?;
        if record.schema != CACHE_SCHEMA || record.manifest_digest != manifest_digest {
            return Err(CacheError::InvalidCache(format!(
                "entry {} does not describe {manifest_digest}",
                entry.display()
            )));
        }
        decode_cached_pack(&self.root.join("blobs/sha256"), manifest_digest)
    }

    pub fn list(&self) -> Result<Vec<CachedFactPack>, CacheError> {
        let directory = self.root.join("entries/sha256");
        let entries = fs::read_dir(&directory).map_err(|source| CacheError::Io {
            operation: "listing fact-pack cache entries",
            path: directory.clone(),
            source,
        })?;
        let mut digests = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| CacheError::Io {
                operation: "reading fact-pack cache entry",
                path: directory.clone(),
                source,
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(CacheError::InvalidCache(format!(
                    "entry name in {} is not UTF-8",
                    directory.display()
                )));
            };
            let Some(hex) = name.strip_suffix(".toml") else {
                return Err(CacheError::InvalidCache(format!(
                    "unexpected entry {}",
                    entry.path().display()
                )));
            };
            let digest = format!("sha256:{hex}");
            validate_digest(&digest)?;
            digests.push(digest);
        }
        digests.sort();
        digests
            .into_iter()
            .map(|digest| self.load(&digest))
            .collect()
    }

    pub fn resolve(
        &self,
        requirement: &FactPackRequirement,
    ) -> Result<Option<CachedFactPack>, CacheError> {
        let mut matching = self
            .list()?
            .into_iter()
            .filter(|pack| requirement.matches(&pack.manifest))
            .collect::<Vec<_>>();
        let Some(revision) = matching.iter().map(|pack| pack.manifest.revision).max() else {
            return Ok(None);
        };
        matching.retain(|pack| pack.manifest.revision == revision);
        if matching.len() != 1 {
            return Err(CacheError::Ambiguous {
                name: requirement.name.clone(),
                revision,
            });
        }
        Ok(matching.pop())
    }

    fn store_blob(&self, digest: &str, bytes: &[u8]) -> Result<(), CacheError> {
        verify_digest(digest, bytes)?;
        let destination = self.blob_path(digest);
        if destination.is_file() {
            let existing = read(&destination, "reading cached fact-pack blob")?;
            verify_digest(digest, &existing)?;
            return Ok(());
        }
        let directory = destination
            .parent()
            .expect("cache blob paths always have a parent");
        let mut temporary = NamedTempFile::new_in(directory).map_err(|source| CacheError::Io {
            operation: "creating temporary fact-pack blob",
            path: directory.to_path_buf(),
            source,
        })?;
        std::io::Write::write_all(&mut temporary, bytes).map_err(|source| CacheError::Io {
            operation: "writing temporary fact-pack blob",
            path: temporary.path().to_path_buf(),
            source,
        })?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = read(&destination, "reading concurrently cached fact-pack blob")?;
                verify_digest(digest, &existing)
            }
            Err(error) => Err(CacheError::Io {
                operation: "installing fact-pack blob",
                path: destination,
                source: error.error,
            }),
        }
    }

    fn store_entry(&self, manifest_digest: &str) -> Result<(), CacheError> {
        let path = self.entry_path(manifest_digest);
        if path.is_file() {
            self.load(manifest_digest)?;
            return Ok(());
        }
        let source = toml::to_string_pretty(&CacheEntry {
            schema: CACHE_SCHEMA,
            manifest_digest: manifest_digest.to_owned(),
        })?;
        persist_noclobber(&path, source.as_bytes(), "installing fact-pack cache entry")
    }

    fn blob_path(&self, digest: &str) -> PathBuf {
        self.root.join("blobs/sha256").join(digest_hex(digest))
    }

    fn entry_path(&self, digest: &str) -> PathBuf {
        self.root
            .join("entries/sha256")
            .join(format!("{}.toml", digest_hex(digest)))
    }
}

fn prepare_cache_root(root: &Path) -> Result<(), CacheError> {
    if !root.exists() {
        create_dir_all(root)?;
    }
    let marker = root.join(CACHE_MARKER);
    if marker.is_file() {
        let source = read(&marker, "reading fact-pack cache marker")?;
        let marker_value: CacheMarker = parse_toml(&marker, &source)?;
        if marker_value.schema != CACHE_SCHEMA {
            return Err(CacheError::InvalidCache(format!(
                "cache schema is {}, supported schema is {CACHE_SCHEMA}",
                marker_value.schema
            )));
        }
        return Ok(());
    }
    let mut entries = fs::read_dir(root).map_err(|source| CacheError::Io {
        operation: "inspecting fact-pack cache",
        path: root.to_path_buf(),
        source,
    })?;
    if entries.next().is_some() {
        return Err(CacheError::InvalidCache(format!(
            "{} is nonempty and has no {CACHE_MARKER}",
            root.display()
        )));
    }
    let source = toml::to_string_pretty(&CacheMarker {
        schema: CACHE_SCHEMA,
    })?;
    fs::write(&marker, source).map_err(|source| CacheError::Io {
        operation: "writing fact-pack cache marker",
        path: marker,
        source,
    })
}

fn decode_layout(layout: &Path) -> Result<DecodedPack, CacheError> {
    let layout_path = layout.join("oci-layout");
    let marker: OciLayout = read_json(&layout_path)?;
    if marker.image_layout_version != "1.0.0" {
        return Err(CacheError::InvalidLayout(format!(
            "image layout version is {}",
            marker.image_layout_version
        )));
    }
    let index_path = layout.join("index.json");
    let index: OciIndex = read_json(&index_path)?;
    if index.schema_version != 2 || index.media_type != OCI_IMAGE_INDEX_MEDIA_TYPE {
        return Err(CacheError::InvalidLayout(
            "index is not an OCI image index v1".to_owned(),
        ));
    }
    let [manifest_descriptor] = index.manifests.as_slice() else {
        return Err(CacheError::InvalidLayout(
            "index must contain exactly one fact-pack manifest".to_owned(),
        ));
    };
    if manifest_descriptor.media_type != OCI_IMAGE_MANIFEST_MEDIA_TYPE {
        return Err(CacheError::InvalidLayout(format!(
            "index manifest has media type {}",
            manifest_descriptor.media_type
        )));
    }
    let blob_root = layout.join("blobs/sha256");
    let manifest_bytes = read_descriptor(&blob_root, manifest_descriptor)?;
    let decoded = decode_manifest(&blob_root, &manifest_descriptor.digest, &manifest_bytes)?;
    let mut blobs = decoded.blobs;
    blobs.insert(manifest_descriptor.digest.clone(), manifest_bytes);
    Ok(DecodedPack {
        pack: decoded.pack,
        blobs,
    })
}

fn decode_cached_pack(
    blob_root: &Path,
    manifest_digest: &str,
) -> Result<CachedFactPack, CacheError> {
    let manifest_path = blob_path(blob_root, manifest_digest);
    let manifest_bytes = read(&manifest_path, "reading cached OCI manifest")?;
    verify_digest(manifest_digest, &manifest_bytes)?;
    Ok(decode_manifest(blob_root, manifest_digest, &manifest_bytes)?.pack)
}

fn decode_manifest(
    blob_root: &Path,
    manifest_digest: &str,
    manifest_bytes: &[u8],
) -> Result<DecodedPack, CacheError> {
    let manifest: OciManifest =
        serde_json::from_slice(manifest_bytes).map_err(|source| CacheError::OciJson {
            path: blob_path(blob_root, manifest_digest),
            source,
        })?;
    if manifest.schema_version != 2
        || manifest.media_type != OCI_IMAGE_MANIFEST_MEDIA_TYPE
        || manifest.artifact_type != FACT_PACK_ARTIFACT_MEDIA_TYPE
    {
        return Err(CacheError::InvalidLayout(
            "artifact manifest is not an Infact fact pack".to_owned(),
        ));
    }
    if manifest.config.media_type != FACT_PACK_CONFIG_MEDIA_TYPE {
        return Err(CacheError::InvalidLayout(format!(
            "config has media type {}",
            manifest.config.media_type
        )));
    }
    let config_bytes = read_descriptor(blob_root, &manifest.config)?;
    let config_source = std::str::from_utf8(&config_bytes)
        .map_err(|_| CacheError::InvalidLayout("fact-pack config is not UTF-8 TOML".to_owned()))?;
    let pack = FactPackManifest::parse(config_source)?;
    if pack.to_canonical_toml()?.as_bytes() != config_bytes {
        return Err(CacheError::InvalidLayout(
            "fact-pack config is not canonical TOML".to_owned(),
        ));
    }
    if pack.contents.len() != manifest.layers.len() {
        return Err(CacheError::InvalidLayout(format!(
            "manifest has {} layers but pack.toml declares {} contents",
            manifest.layers.len(),
            pack.contents.len()
        )));
    }
    let mut blobs = BTreeMap::from([(manifest.config.digest.clone(), config_bytes)]);
    let mut contents = Vec::with_capacity(pack.contents.len());
    for (content, descriptor) in pack.contents.iter().zip(&manifest.layers) {
        let title = descriptor.annotations.get("org.opencontainers.image.title");
        if title != Some(&content.path)
            || descriptor.media_type != content.media_type
            || descriptor.digest != content.sha256
        {
            return Err(CacheError::InvalidLayout(format!(
                "OCI layer does not match declared content {}",
                content.path
            )));
        }
        let bytes = read_descriptor(blob_root, descriptor)?;
        blobs.insert(descriptor.digest.clone(), bytes);
        contents.push(CachedContent {
            path: content.path.clone(),
            kind: content.kind.clone(),
            media_type: descriptor.media_type.clone(),
            digest: descriptor.digest.clone(),
            size: descriptor.size,
            blob_path: blob_path(blob_root, &descriptor.digest),
        });
    }
    Ok(DecodedPack {
        pack: CachedFactPack {
            manifest: pack,
            manifest_digest: manifest_digest.to_owned(),
            config_digest: manifest.config.digest,
            contents,
        },
        blobs,
    })
}

fn read_descriptor(blob_root: &Path, descriptor: &OciDescriptor) -> Result<Vec<u8>, CacheError> {
    validate_digest(&descriptor.digest)?;
    let path = blob_path(blob_root, &descriptor.digest);
    let bytes = read(&path, "reading OCI blob")?;
    if bytes.len() != descriptor.size {
        return Err(CacheError::BlobSize {
            digest: descriptor.digest.clone(),
            expected: descriptor.size,
            actual: bytes.len(),
        });
    }
    verify_digest(&descriptor.digest, &bytes)?;
    Ok(bytes)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CacheError> {
    let source = read(path, "reading OCI metadata")?;
    serde_json::from_slice(&source).map_err(|source| CacheError::OciJson {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_toml<T: serde::de::DeserializeOwned>(path: &Path, source: &[u8]) -> Result<T, CacheError> {
    let source = std::str::from_utf8(source)
        .map_err(|_| CacheError::InvalidCache(format!("{} is not UTF-8 TOML", path.display())))?;
    toml::from_str(source).map_err(|source| CacheError::CacheToml {
        path: path.to_path_buf(),
        source,
    })
}

fn read(path: &Path, operation: &'static str) -> Result<Vec<u8>, CacheError> {
    fs::read(path).map_err(|source| CacheError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })
}

fn verify_digest(expected: &str, bytes: &[u8]) -> Result<(), CacheError> {
    validate_digest(expected)?;
    let actual = sha256(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(CacheError::BlobDigest {
            expected: expected.to_owned(),
            actual,
        })
    }
}

pub(super) fn validate_digest(digest: &str) -> Result<(), CacheError> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(CacheError::InvalidLayout(format!(
            "digest {digest} does not use SHA-256"
        )));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CacheError::InvalidLayout(format!(
            "digest {digest} is not lowercase SHA-256"
        )));
    }
    Ok(())
}

fn digest_hex(digest: &str) -> &str {
    digest
        .strip_prefix("sha256:")
        .expect("validated digests have a SHA-256 prefix")
}

fn blob_path(blob_root: &Path, digest: &str) -> PathBuf {
    blob_root.join(digest_hex(digest))
}

fn create_dir_all(path: &Path) -> Result<(), CacheError> {
    fs::create_dir_all(path).map_err(|source| CacheError::Io {
        operation: "creating fact-pack cache directory",
        path: path.to_path_buf(),
        source,
    })
}

fn persist_noclobber(
    destination: &Path,
    bytes: &[u8],
    operation: &'static str,
) -> Result<(), CacheError> {
    let directory = destination
        .parent()
        .expect("cache paths always have a parent");
    let mut temporary = NamedTempFile::new_in(directory).map_err(|source| CacheError::Io {
        operation: "creating temporary fact-pack cache file",
        path: directory.to_path_buf(),
        source,
    })?;
    std::io::Write::write_all(&mut temporary, bytes).map_err(|source| CacheError::Io {
        operation: "writing temporary fact-pack cache file",
        path: temporary.path().to_path_buf(),
        source,
    })?;
    match temporary.persist_noclobber(destination) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(CacheError::Io {
            operation,
            path: destination.to_path_buf(),
            source: error.error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        Content, Derivation, FACT_PACK_SCHEMA, SourceKind, SubjectKind, build_oci_layout, sha256,
    };

    fn fixture(root: &Path, revision: u32) -> FactPackManifest {
        fs::create_dir_all(root.join("behaviors")).unwrap();
        fs::write(root.join("behaviors/counts.json"), b"{}\n").unwrap();
        FactPackManifest {
            schema: FACT_PACK_SCHEMA,
            name: "rust-itertools".to_owned(),
            revision,
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
                sha256: format!("sha256:{}", "0".repeat(64)),
            }],
            derivation: Derivation {
                generator: "infact".to_owned(),
                generator_version: "0.0.0".to_owned(),
                analyzer_sha256: format!("sha256:{}", "1".repeat(64)),
            },
            compatibility: Compatibility::default(),
            provides: BTreeSet::from(["rust.library-behaviors".to_owned()]),
            requires: BTreeSet::from(["rust.syntax-tree".to_owned()]),
            contents: vec![Content {
                path: "behaviors/counts.json".to_owned(),
                kind: "library-behavior".to_owned(),
                media_type: "application/vnd.infact.library-behavior.v1+json".to_owned(),
                sha256: sha256(b"{}\n"),
            }],
        }
    }

    #[test]
    fn imports_loads_and_resolves_a_layout_idempotently() {
        let pack_root = tempfile::tempdir().unwrap();
        let manifest = fixture(pack_root.path(), 1);
        let layout_root = tempfile::tempdir().unwrap();
        let layout = layout_root.path().join("layout");
        let built = build_oci_layout(&manifest, pack_root.path(), &layout).unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        let cache_path = cache_root.path().join("cache");
        let cache = FactPackCache::open(&cache_path).unwrap();

        let imported = cache.import_oci_layout(&layout).unwrap();
        assert_eq!(imported.manifest_digest, built.manifest_digest);
        assert_eq!(cache.load(&built.manifest_digest).unwrap(), imported);
        assert_eq!(cache.import_oci_layout(&layout).unwrap(), imported);
        assert_eq!(cache.list().unwrap(), vec![imported.clone()]);
        assert_eq!(
            cache
                .resolve(&FactPackRequirement::from_manifest(&manifest))
                .unwrap(),
            Some(imported)
        );
    }

    #[test]
    fn rejects_a_tampered_layout_blob() {
        let pack_root = tempfile::tempdir().unwrap();
        let manifest = fixture(pack_root.path(), 1);
        let layout_root = tempfile::tempdir().unwrap();
        let layout = layout_root.path().join("layout");
        build_oci_layout(&manifest, pack_root.path(), &layout).unwrap();
        let content = &manifest.contents[0];
        fs::write(
            layout
                .join("blobs/sha256")
                .join(digest_hex(&content.sha256)),
            b"tampered\n",
        )
        .unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let error = cache.import_oci_layout(layout).unwrap_err().to_string();
        assert!(error.contains("has size") || error.contains("has digest"));
    }

    #[test]
    fn resolves_the_highest_compatible_revision() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let mut manifests = Vec::new();
        for revision in [1, 2] {
            let pack_root = tempfile::tempdir().unwrap();
            let manifest = fixture(pack_root.path(), revision);
            let layout_root = tempfile::tempdir().unwrap();
            let layout = layout_root.path().join("layout");
            build_oci_layout(&manifest, pack_root.path(), &layout).unwrap();
            cache.import_oci_layout(layout).unwrap();
            manifests.push(manifest);
        }

        let requirement = FactPackRequirement::from_manifest(&manifests[0]);
        let resolved = cache.resolve(&requirement).unwrap().unwrap();
        assert_eq!(resolved.manifest.revision, 2);

        let mut unavailable = requirement;
        unavailable.derivation.analyzer_sha256 = format!("sha256:{}", "9".repeat(64));
        assert_eq!(cache.resolve(&unavailable).unwrap(), None);
    }

    #[test]
    fn imports_a_downloaded_artifact_without_an_image_layout() {
        let pack_root = tempfile::tempdir().unwrap();
        let manifest = fixture(pack_root.path(), 1);
        let layout_root = tempfile::tempdir().unwrap();
        let layout = layout_root.path().join("layout");
        let built = build_oci_layout(&manifest, pack_root.path(), &layout).unwrap();
        let blob_root = layout.join("blobs/sha256");
        let manifest_bytes = fs::read(blob_path(&blob_root, &built.manifest_digest)).unwrap();
        let wire_manifest: OciManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let mut blobs = BTreeMap::new();
        for descriptor in std::iter::once(&wire_manifest.config).chain(&wire_manifest.layers) {
            blobs.insert(
                descriptor.digest.clone(),
                fs::read(blob_path(&blob_root, &descriptor.digest)).unwrap(),
            );
        }
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();

        let imported = cache
            .import_oci_artifact(&built.manifest_digest, &manifest_bytes, &blobs)
            .unwrap();
        assert_eq!(imported.manifest, manifest);
        assert_eq!(imported.manifest_digest, built.manifest_digest);
    }

    #[test]
    fn rejects_a_nonempty_unmarked_cache_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("unrelated"), b"data").unwrap();
        assert!(
            FactPackCache::open(root.path())
                .unwrap_err()
                .to_string()
                .contains("nonempty")
        );
    }
}
