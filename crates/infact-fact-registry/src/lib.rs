//! OCI Distribution transport for verified Infact fact packs.

use std::collections::BTreeMap;

use infact_fact_pack::{CacheError, CachedFactPack, FACT_PACK_ARTIFACT_MEDIA_TYPE, FactPackCache};
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryLimits {
    pub max_manifest_bytes: usize,
    pub max_blob_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 4 * 1024 * 1024,
            max_blob_bytes: 256 * 1024 * 1024,
            max_total_bytes: 1024 * 1024 * 1024,
        }
    }
}

pub enum FactPackRegistryAuth {
    Anonymous,
    Basic { username: String, password: String },
    Bearer(String),
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid OCI reference {reference}: {source}")]
    Reference {
        reference: String,
        #[source]
        source: oci_client::ParseError,
    },
    #[error("pulling fact pack {reference}: {source}")]
    Pull {
        reference: String,
        #[source]
        source: oci_client::errors::OciDistributionError,
    },
    #[error("parsing fact-pack OCI manifest: {0}")]
    ManifestJson(#[from] serde_json::Error),
    #[error("OCI artifact is {actual}, expected {expected}")]
    DigestMismatch { expected: String, actual: String },
    #[error("OCI artifact type is {0}, expected an Infact fact pack")]
    ArtifactType(String),
    #[error("OCI descriptor {digest} has invalid size {size}")]
    InvalidSize { digest: String, size: i64 },
    #[error("OCI {kind} exceeds the configured {limit}-byte limit")]
    Limit { kind: &'static str, limit: usize },
    #[error(transparent)]
    Cache(#[from] CacheError),
}

pub struct FactPackRegistry {
    client: Client,
    limits: RegistryLimits,
}

impl Default for FactPackRegistry {
    fn default() -> Self {
        Self::new(RegistryLimits::default())
    }
}

impl FactPackRegistry {
    pub fn new(limits: RegistryLimits) -> Self {
        Self {
            client: Client::new(ClientConfig::default()),
            limits,
        }
    }

    /// Construct a client that permits plain HTTP. Intended for local registries.
    pub fn local_http(limits: RegistryLimits) -> Self {
        let config = ClientConfig {
            protocol: ClientProtocol::Http,
            ..ClientConfig::default()
        };
        Self {
            client: Client::new(config),
            limits,
        }
    }

    pub async fn pull(
        &self,
        reference: &str,
        auth: &FactPackRegistryAuth,
        expected_digest: Option<&str>,
        cache: &FactPackCache,
    ) -> Result<CachedFactPack, RegistryError> {
        let parsed: Reference = reference
            .parse()
            .map_err(|source| RegistryError::Reference {
                reference: reference.to_owned(),
                source,
            })?;
        let auth = registry_auth(auth);
        let (manifest_bytes, manifest_digest) = self
            .client
            .pull_manifest_raw(&parsed, &auth, &[OCI_IMAGE_MEDIA_TYPE])
            .await
            .map_err(|source| RegistryError::Pull {
                reference: reference.to_owned(),
                source,
            })?;
        if manifest_bytes.len() > self.limits.max_manifest_bytes {
            return Err(RegistryError::Limit {
                kind: "manifest",
                limit: self.limits.max_manifest_bytes,
            });
        }
        if let Some(expected) = expected_digest
            && manifest_digest != expected
        {
            return Err(RegistryError::DigestMismatch {
                expected: expected.to_owned(),
                actual: manifest_digest,
            });
        }
        let manifest: OciImageManifest = serde_json::from_slice(&manifest_bytes)?;
        if manifest.artifact_type.as_deref() != Some(FACT_PACK_ARTIFACT_MEDIA_TYPE) {
            return Err(RegistryError::ArtifactType(
                manifest
                    .artifact_type
                    .unwrap_or_else(|| "<missing>".to_owned()),
            ));
        }
        let descriptors = std::iter::once(&manifest.config)
            .chain(&manifest.layers)
            .collect::<Vec<_>>();
        let mut total = manifest_bytes.len();
        for descriptor in &descriptors {
            let size =
                usize::try_from(descriptor.size).map_err(|_| RegistryError::InvalidSize {
                    digest: descriptor.digest.clone(),
                    size: descriptor.size,
                })?;
            if size > self.limits.max_blob_bytes {
                return Err(RegistryError::Limit {
                    kind: "blob",
                    limit: self.limits.max_blob_bytes,
                });
            }
            total = total.checked_add(size).ok_or(RegistryError::Limit {
                kind: "artifact",
                limit: self.limits.max_total_bytes,
            })?;
            if total > self.limits.max_total_bytes {
                return Err(RegistryError::Limit {
                    kind: "artifact",
                    limit: self.limits.max_total_bytes,
                });
            }
        }

        let mut blobs = BTreeMap::new();
        for descriptor in descriptors {
            if blobs.contains_key(&descriptor.digest) {
                continue;
            }
            let mut bytes = Vec::new();
            self.client
                .pull_blob(&parsed, descriptor, &mut bytes)
                .await
                .map_err(|source| RegistryError::Pull {
                    reference: reference.to_owned(),
                    source,
                })?;
            blobs.insert(descriptor.digest.clone(), bytes);
        }
        cache
            .import_oci_artifact(&manifest_digest, &manifest_bytes, &blobs)
            .map_err(Into::into)
    }

    pub async fn list_tags(
        &self,
        reference: &str,
        auth: &FactPackRegistryAuth,
    ) -> Result<Vec<String>, RegistryError> {
        let parsed: Reference = reference
            .parse()
            .map_err(|source| RegistryError::Reference {
                reference: reference.to_owned(),
                source,
            })?;
        let mut tags = self
            .client
            .list_tags(&parsed, &registry_auth(auth), None, None)
            .await
            .map_err(|source| RegistryError::Pull {
                reference: reference.to_owned(),
                source,
            })?
            .tags;
        tags.sort();
        tags.dedup();
        Ok(tags)
    }
}

fn registry_auth(auth: &FactPackRegistryAuth) -> RegistryAuth {
    match auth {
        FactPackRegistryAuth::Anonymous => RegistryAuth::Anonymous,
        FactPackRegistryAuth::Basic { username, password } => {
            RegistryAuth::Basic(username.clone(), password.clone())
        }
        FactPackRegistryAuth::Bearer(token) => RegistryAuth::Bearer(token.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use infact_fact_pack::{
        Compatibility, Content, Derivation, FACT_PACK_SCHEMA, FactPackManifest, SourceInput,
        SourceKind, Subject, SubjectKind, build_oci_layout, sha256,
    };

    #[test]
    fn limits_are_bounded_by_default() {
        let limits = RegistryLimits::default();
        assert!(limits.max_manifest_bytes < limits.max_blob_bytes);
        assert!(limits.max_blob_bytes < limits.max_total_bytes);
    }

    #[test]
    fn rejects_an_invalid_reference_before_network_access() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let result = runtime.block_on(FactPackRegistry::default().pull(
            "not a reference",
            &FactPackRegistryAuth::Anonymous,
            None,
            &cache,
        ));
        assert!(matches!(result, Err(RegistryError::Reference { .. })));
    }

    #[test]
    fn pulls_a_fact_pack_from_an_oci_distribution_endpoint() {
        let pack_root = tempfile::tempdir().unwrap();
        let manifest = fixture(pack_root.path());
        let layout_root = tempfile::tempdir().unwrap();
        let layout = layout_root.path().join("layout");
        let built = build_oci_layout(&manifest, pack_root.path(), &layout).unwrap();
        let blob_root = layout.join("blobs/sha256");
        let manifest_bytes =
            fs::read(blob_root.join(built.manifest_digest.trim_start_matches("sha256:"))).unwrap();
        let wire: OciImageManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let blobs = std::iter::once(&wire.config)
            .chain(&wire.layers)
            .map(|descriptor| {
                (
                    descriptor.digest.clone(),
                    fs::read(blob_root.join(descriptor.digest.trim_start_matches("sha256:")))
                        .unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = Arc::clone(&stopped);
        let server_digest = built.manifest_digest.clone();
        let server = thread::spawn(move || {
            serve_registry(
                listener,
                &server_digest,
                &manifest_bytes,
                &blobs,
                &server_stopped,
            );
        });
        let cache_root = tempfile::tempdir().unwrap();
        let cache = FactPackCache::open(cache_root.path().join("cache")).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let registry = FactPackRegistry::local_http(RegistryLimits::default());
        let tags = runtime
            .block_on(registry.list_tags(
                &format!("{address}/test/facts:latest"),
                &FactPackRegistryAuth::Anonymous,
            ))
            .unwrap();
        let result = runtime.block_on(registry.pull(
            &format!("{address}/test/facts:latest"),
            &FactPackRegistryAuth::Anonymous,
            Some(&built.manifest_digest),
            &cache,
        ));
        stopped.store(true, Ordering::Release);
        // a throwaway connection so the accept loop observes `stopped` and returns
        #[allow(clippy::let_underscore_must_use)]
        let _ = TcpStream::connect(address);
        server.join().unwrap();
        let pulled = result.unwrap();
        assert_eq!(tags, ["1.93.1-r1", "latest"]);
        assert_eq!(pulled.manifest, manifest);
        assert_eq!(pulled.manifest_digest, built.manifest_digest);
    }

    fn fixture(root: &Path) -> FactPackManifest {
        fs::create_dir_all(root.join("effects")).unwrap();
        fs::write(root.join("effects/core.json"), b"{}\n").unwrap();
        FactPackManifest {
            schema: FACT_PACK_SCHEMA,
            name: "rust-core".to_owned(),
            revision: 1,
            subject: Subject {
                kind: SubjectKind::Language,
                language: "rust".to_owned(),
                ecosystem: Some("cargo".to_owned()),
                name: "core".to_owned(),
                version: "1.93.1".to_owned(),
            },
            sources: vec![SourceInput {
                kind: SourceKind::Toolchain,
                name: "rust".to_owned(),
                version: "1.93.1".to_owned(),
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
                path: "effects/core.json".to_owned(),
                kind: "effects".to_owned(),
                media_type: "application/vnd.infact.effects.v1+json".to_owned(),
                sha256: sha256(b"{}\n"),
            }],
        }
    }

    fn serve_registry(
        listener: TcpListener,
        manifest_digest: &str,
        manifest: &[u8],
        blobs: &BTreeMap<String, Vec<u8>>,
        stopped: &AtomicBool,
    ) {
        listener.set_nonblocking(true).unwrap();
        while !stopped.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("accepting registry request: {error}"),
            };
            // The listener is non-blocking so this loop can notice the stop
            // flag, and an accepted connection inherits that on some platforms.
            // Reading a request is not something to poll, so put it back.
            stream
                .set_nonblocking(false)
                .expect("serving a connection blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("bounding a stuck request");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            if request.is_empty() {
                continue;
            }
            let request = String::from_utf8(request).unwrap();
            let path = request.split_whitespace().nth(1).unwrap();
            if path == "/v2/" {
                respond(&mut stream, "application/json", None, b"{}");
            } else if path == "/v2/test/facts/tags/list" {
                respond(
                    &mut stream,
                    "application/json",
                    None,
                    br#"{"name":"test/facts","tags":["latest","1.93.1-r1","latest"]}"#,
                );
            } else if path == "/v2/test/facts/manifests/latest" {
                respond(
                    &mut stream,
                    OCI_IMAGE_MEDIA_TYPE,
                    Some(manifest_digest),
                    manifest,
                );
            } else if let Some(digest) = path.strip_prefix("/v2/test/facts/blobs/") {
                let bytes = blobs
                    .get(digest)
                    .unwrap_or_else(|| panic!("unknown {path}"));
                respond(&mut stream, "application/octet-stream", Some(digest), bytes);
            } else {
                panic!("unexpected registry request {path}");
            }
        }
    }

    fn respond(stream: &mut TcpStream, media_type: &str, digest: Option<&str>, body: &[u8]) {
        let digest = digest
            .map(|digest| format!("Docker-Content-Digest: {digest}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {media_type}\r\nContent-Length: {}\r\n{digest}Connection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }
}
