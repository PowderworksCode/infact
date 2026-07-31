use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use super::{
    CacheError, CachedFactPack, FactPackCache, FactPackManifest, FactPackRequirement,
    ManifestError, cache::validate_digest, sha256,
};

pub const FACT_PACK_LOCK_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FactPackLock {
    pub schema: u32,
    #[serde(default)]
    pub packs: Vec<LockedFactPack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LockedFactPack {
    pub manifest_digest: String,
    pub origin: Option<String>,
    pub manifest: FactPackManifest,
}

#[derive(Debug, Error)]
pub enum FactPackLockError {
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("parsing fact-pack lock {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing fact-pack lock: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("{operation} {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid fact-pack lock: {0}")]
    Invalid(String),
    #[error("fact-pack lock has no entry satisfying {name}")]
    Missing { name: String },
    #[error("fact-pack lock has multiple entries satisfying {name}")]
    Ambiguous { name: String },
    #[error("fact-pack lock entry {digest} does not match the cached manifest")]
    Drift { digest: String },
    #[error("fact-pack lock contains entries not requested by the current repository: {names}")]
    Extra { names: String },
}

impl Default for FactPackLock {
    fn default() -> Self {
        Self {
            schema: FACT_PACK_LOCK_SCHEMA,
            packs: Vec::new(),
        }
    }
}

impl FactPackLock {
    pub fn parse(path: &Path, source: &str) -> Result<Self, FactPackLockError> {
        let lock: Self = toml::from_str(source).map_err(|source| FactPackLockError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        lock.validate()?;
        Ok(lock)
    }

    pub fn read(path: &Path) -> Result<Self, FactPackLockError> {
        let source = fs::read_to_string(path).map_err(|source| FactPackLockError::Io {
            operation: "reading fact-pack lock",
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &source)
    }

    pub fn read_or_default(path: &Path) -> Result<Self, FactPackLockError> {
        match fs::read_to_string(path) {
            Ok(source) => Self::parse(path, &source),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(FactPackLockError::Io {
                operation: "reading fact-pack lock",
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn validate(&self) -> Result<(), FactPackLockError> {
        if self.schema != FACT_PACK_LOCK_SCHEMA {
            return Err(FactPackLockError::Invalid(format!(
                "schema is {}, supported schema is {FACT_PACK_LOCK_SCHEMA}",
                self.schema
            )));
        }
        let mut previous = None;
        for entry in &self.packs {
            validate_digest(&entry.manifest_digest)?;
            entry.manifest.validate()?;
            if entry.origin.as_ref().is_some_and(String::is_empty) {
                return Err(FactPackLockError::Invalid(
                    "pack origin cannot be empty".to_owned(),
                ));
            }
            let key = requirement_digest(&entry.manifest)?;
            if previous.as_ref().is_some_and(|previous| previous >= &key) {
                return Err(FactPackLockError::Invalid(
                    "packs must be sorted by unique requirement digest".to_owned(),
                ));
            }
            previous = Some(key);
        }
        Ok(())
    }

    pub fn insert(
        &mut self,
        pack: &CachedFactPack,
        origin: Option<String>,
    ) -> Result<(), FactPackLockError> {
        self.validate()?;
        pack.manifest.validate()?;
        let key = requirement_digest(&pack.manifest)?;
        self.packs.retain(|entry| {
            requirement_digest(&entry.manifest)
                .expect("validated lock entries have a requirement digest")
                != key
        });
        self.packs.push(LockedFactPack {
            manifest_digest: pack.manifest_digest.clone(),
            origin,
            manifest: pack.manifest.clone(),
        });
        self.packs.sort_by_cached_key(|entry| {
            requirement_digest(&entry.manifest)
                .expect("validated cached manifests have a requirement digest")
        });
        self.validate()
    }

    pub fn to_canonical_toml(&self) -> Result<String, FactPackLockError> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn write(&self, path: &Path) -> Result<(), FactPackLockError> {
        let source = self.to_canonical_toml()?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(directory).map_err(|source| FactPackLockError::Io {
            operation: "creating fact-pack lock directory",
            path: directory.to_path_buf(),
            source,
        })?;
        let mut temporary =
            NamedTempFile::new_in(directory).map_err(|source| FactPackLockError::Io {
                operation: "creating temporary fact-pack lock",
                path: directory.to_path_buf(),
                source,
            })?;
        std::io::Write::write_all(&mut temporary, source.as_bytes()).map_err(|source| {
            FactPackLockError::Io {
                operation: "writing temporary fact-pack lock",
                path: temporary.path().to_path_buf(),
                source,
            }
        })?;
        temporary
            .persist(path)
            .map_err(|error| FactPackLockError::Io {
                operation: "installing fact-pack lock",
                path: path.to_path_buf(),
                source: error.error,
            })?;
        Ok(())
    }

    pub fn verify(&self, cache: &FactPackCache) -> Result<Vec<CachedFactPack>, FactPackLockError> {
        self.validate()?;
        self.packs
            .iter()
            .map(|entry| load_locked(cache, entry))
            .collect()
    }

    pub fn resolve(
        &self,
        cache: &FactPackCache,
        requirements: &[FactPackRequirement],
    ) -> Result<Vec<CachedFactPack>, FactPackLockError> {
        self.validate()?;
        let mut used = BTreeSet::new();
        let mut resolved = Vec::with_capacity(requirements.len());
        for requirement in requirements {
            let matching = self
                .packs
                .iter()
                .filter(|entry| requirement.matches(&entry.manifest))
                .collect::<Vec<_>>();
            let entry = match matching.as_slice() {
                [] => {
                    return Err(FactPackLockError::Missing {
                        name: requirement.name.clone(),
                    });
                }
                [entry] => *entry,
                _ => {
                    return Err(FactPackLockError::Ambiguous {
                        name: requirement.name.clone(),
                    });
                }
            };
            if !used.insert(entry.manifest_digest.clone()) {
                return Err(FactPackLockError::Ambiguous {
                    name: requirement.name.clone(),
                });
            }
            resolved.push(load_locked(cache, entry)?);
        }
        if used.len() != self.packs.len() {
            let names = self
                .packs
                .iter()
                .filter(|entry| !used.contains(&entry.manifest_digest))
                .map(|entry| entry.manifest.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(FactPackLockError::Extra { names });
        }
        Ok(resolved)
    }
}

fn load_locked(
    cache: &FactPackCache,
    entry: &LockedFactPack,
) -> Result<CachedFactPack, FactPackLockError> {
    let pack = cache.load(&entry.manifest_digest)?;
    if pack.manifest != entry.manifest {
        return Err(FactPackLockError::Drift {
            digest: entry.manifest_digest.clone(),
        });
    }
    Ok(pack)
}

fn requirement_digest(manifest: &FactPackManifest) -> Result<String, ManifestError> {
    let requirement = FactPackRequirement::from_manifest(manifest);
    let source = toml::to_string(&requirement)?;
    Ok(sha256(source.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use super::*;
    use crate::{
        Compatibility, Content, Derivation, FACT_PACK_SCHEMA, SourceInput, SourceKind, Subject,
        SubjectKind, build_oci_layout,
    };

    fn install(cache: &FactPackCache, root: &Path, name: &str) -> CachedFactPack {
        fs::create_dir_all(root.join("facts")).unwrap();
        fs::write(root.join("facts/data.json"), b"{}\n").unwrap();
        let manifest = FactPackManifest {
            schema: FACT_PACK_SCHEMA,
            name: name.to_owned(),
            revision: 1,
            subject: Subject {
                kind: SubjectKind::Library,
                language: "rust".to_owned(),
                ecosystem: Some("cargo".to_owned()),
                name: name.trim_start_matches("rust-").to_owned(),
                version: "1.0.0".to_owned(),
            },
            sources: vec![SourceInput {
                kind: SourceKind::Package,
                name: name.to_owned(),
                version: "1.0.0".to_owned(),
                sha256: format!("sha256:{}", "0".repeat(64)),
            }],
            derivation: Derivation {
                generator: "infact".to_owned(),
                generator_version: "0.0.0".to_owned(),
                analyzer_sha256: format!("sha256:{}", "1".repeat(64)),
            },
            compatibility: Compatibility::default(),
            provides: BTreeSet::from(["rust.effects".to_owned()]),
            requires: BTreeSet::new(),
            contents: vec![Content {
                path: "facts/data.json".to_owned(),
                kind: "effects".to_owned(),
                media_type: "application/vnd.infact.effects.v1+json".to_owned(),
                sha256: sha256(b"{}\n"),
            }],
        };
        let layout_root = tempfile::tempdir().unwrap();
        let layout = layout_root.path().join("layout");
        build_oci_layout(&manifest, root, &layout).unwrap();
        cache.import_oci_layout(layout).unwrap()
    }

    #[test]
    fn lock_round_trips_and_verifies_cached_manifests() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let pack_root = tempfile::tempdir().unwrap();
        let pack = install(&cache, pack_root.path(), "rust-core");
        let mut lock = FactPackLock::default();
        lock.insert(&pack, Some("ghcr.io/acme/rust-core:1.0.0-r1".to_owned()))
            .unwrap();
        let lock_root = tempfile::tempdir().unwrap();
        let path = lock_root.path().join("infact.lock.toml");
        lock.write(&path).unwrap();

        let read = FactPackLock::read(&path).unwrap();
        assert_eq!(read, lock);
        let second = lock_root.path().join("second.lock.toml");
        read.write(&second).unwrap();
        assert_eq!(fs::read(&path).unwrap(), fs::read(second).unwrap());
        assert_eq!(read.verify(&cache).unwrap(), vec![pack.clone()]);
        assert_eq!(
            read.resolve(
                &cache,
                &[FactPackRequirement::from_manifest(&pack.manifest)]
            )
            .unwrap(),
            vec![pack]
        );
    }

    #[test]
    fn locked_resolution_rejects_configuration_disagreement() {
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let first = install(&cache, first_root.path(), "rust-core");
        let second = install(&cache, second_root.path(), "rust-itertools");
        let mut lock = FactPackLock::default();
        lock.insert(&first, None).unwrap();
        lock.insert(&second, None).unwrap();

        let error = lock
            .resolve(
                &cache,
                &[FactPackRequirement::from_manifest(&first.manifest)],
            )
            .unwrap_err();
        assert!(matches!(error, FactPackLockError::Extra { .. }));
    }

    #[test]
    fn offline_verification_rejects_a_missing_cached_artifact() {
        let populated_root = tempfile::tempdir().unwrap();
        let populated = FactPackCache::open(populated_root.path().join("cache")).unwrap();
        let pack_root = tempfile::tempdir().unwrap();
        let pack = install(&populated, pack_root.path(), "rust-core");
        let mut lock = FactPackLock::default();
        lock.insert(&pack, None).unwrap();
        let empty_root = tempfile::tempdir().unwrap();
        let empty = FactPackCache::open(empty_root.path().join("cache")).unwrap();

        let error = lock.verify(&empty).unwrap_err();
        assert!(matches!(
            error,
            FactPackLockError::Cache(CacheError::NotInstalled { .. })
        ));
    }
}
