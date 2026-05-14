// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Capability probe + cache for the OCI Referrers API.
//!
//! A registry either supports the Referrers API (`/v2/<name>/referrers/<digest>`)
//! or it does not. OCX probes the endpoint once and caches the result per
//! registry. The cache is consulted before each referrer operation; TTL is
//! 6 hours.
//!
//! See [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! §"Capability cache".

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::oci::client::OciTransport;
use crate::oci::client::error::ClientError;
use crate::oci::{Digest, native};
use crate::prelude::StringExt;

/// Cache TTL: 6 hours.
const TTL_SECS: u64 = 6 * 3600;

/// Referrers-API support state for a given registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferrersSupport {
    /// Registry returned 200 on `/v2/<name>/referrers/<digest>`.
    Supported,
    /// Registry returned 404/405 on the referrers endpoint.
    Unsupported,
}

/// Cached probe result for a single registry.
///
/// Stored at `{ocx_home}/state/referrers/{registry_slug}.json`. The
/// cache is advisory and fail-open: a corrupt file is treated as a cache miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferrersApiCapability {
    /// Registry hostname (e.g., `ghcr.io`) this capability applies to.
    pub registry: String,

    /// Support state from the last probe.
    pub supported: ReferrersSupport,

    /// Wall-clock time of the probe (UTC).
    pub probed_at: SystemTime,

    /// TTL in seconds; caller compares `probed_at + ttl` to now.
    pub ttl_seconds: u64,
}

impl ReferrersApiCapability {
    /// Probe the registry's Referrers API support.
    ///
    /// Issues `GET /v2/{repo}/referrers/{digest}` via the transport by calling
    /// [`OciTransport::list_referrers`]:
    ///
    /// - `Ok(_)` (including an empty list) → [`ReferrersSupport::Supported`]
    /// - [`ClientError::ReferrersUnsupported`] → [`ReferrersSupport::Unsupported`]
    /// - Any other error → propagated to the caller
    ///
    /// The `registry`, `repo`, and `digest` parameters identify the endpoint
    /// to probe. A zero-value or otherwise known digest is acceptable — the
    /// response body is ignored; only the HTTP status matters.
    ///
    /// # Probe digest caveat
    ///
    /// Callers that pass a synthetic all-zero digest (`sha256:000…0`) should
    /// expect some registries to validate digest format before dispatch and
    /// return 400 rather than 200 or 404. That path currently surfaces as a
    /// non-`ReferrersUnsupported` `ClientError` — the caller decides whether
    /// to treat the probe as inconclusive. Prefer passing the subject
    /// manifest's real digest when available.
    pub async fn probe(
        transport: &dyn OciTransport,
        registry: &str,
        repo: &str,
        digest: &Digest,
    ) -> Result<Self, ClientError> {
        // Build a reference pointing at the registry + repo. The tag is
        // irrelevant for a referrers probe (the endpoint uses the digest
        // directly), but the transport requires a well-formed reference.
        let image = native::Reference::with_tag(registry.to_string(), repo.to_string(), "latest".to_string());

        let supported = match transport.list_referrers(&image, digest).await {
            Ok(_) => ReferrersSupport::Supported,
            Err(ClientError::ReferrersUnsupported { .. }) => ReferrersSupport::Unsupported,
            Err(other) => return Err(other),
        };

        Ok(Self {
            registry: registry.to_string(),
            supported,
            probed_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        })
    }

    /// Persist the capability record atomically to
    /// `{ocx_home}/state/referrers/{registry_slug}.json`.
    ///
    /// Uses a temporary file + rename for atomicity so a concurrent reader
    /// never sees a partially-written file. The rename step uses
    /// `std::fs::rename`, which is replace-existing on both POSIX
    /// (`rename(2)`) and Windows (`MoveFileExW` with
    /// `MOVEFILE_REPLACE_EXISTING`), so repeated writes for the same
    /// registry overwrite the previous cache atomically on every platform.
    pub async fn write_cache(&self, cache_root: &Path) -> io::Result<()> {
        let target = cache_path(cache_root, &self.registry);
        let dir = target
            .parent()
            .ok_or_else(|| io::Error::other("cache path has no parent"))?
            .to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;

        let bytes = serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Write to a temp file in the same directory so rename is atomic
        // (same mount point guaranteed on POSIX, same volume on Windows).
        // `NamedTempFile` and the final rename both touch blocking file
        // handles; run the whole sequence on a blocking thread.
        //
        // We disarm `NamedTempFile`'s drop-delete by promoting it to a
        // `TempPath` via `into_temp_path()`, then `keep()` to release
        // cleanup, then `std::fs::rename` for the atomic replace-existing
        // step. `NamedTempFile::persist` is not used because it does NOT
        // replace an existing target on Windows.
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            #[cfg(unix)]
            let tmp = {
                use std::os::unix::fs::PermissionsExt;
                tempfile::Builder::new()
                    .permissions(std::fs::Permissions::from_mode(0o600))
                    .tempfile_in(&dir)?
            };
            #[cfg(not(unix))]
            let tmp = tempfile::Builder::new().tempfile_in(&dir)?;
            std::fs::write(tmp.path(), &bytes)?;
            // Release the tempfile's drop-delete guard before renaming so
            // a successful rename does not race with cleanup.
            let tmp_path = tmp.into_temp_path().keep().map_err(io::Error::other)?;
            std::fs::rename(&tmp_path, &target)?;
            Ok(())
        })
        .await
        .map_err(|e| io::Error::other(format!("write_cache tempfile+rename panicked: {e}")))??;
        Ok(())
    }

    /// Read a cached capability from disk without probing.
    ///
    /// Returns `Ok(None)` when the cache file is missing, expired, or
    /// corrupt (fail-open). Returns `Ok(Some(_))` when a fresh entry is
    /// available.
    ///
    /// Fail-open on corrupt/invalid content is deliberate: a corrupt cache
    /// should never turn into a signing/verification failure. The caller
    /// falls back to probe, the probe overwrites the corrupt file, the next
    /// call reads the freshly written one.
    pub async fn from_cache(registry: &str, cache_root: &Path) -> io::Result<Option<Self>> {
        let path = cache_path(cache_root, registry);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let capability: Self = match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        if capability.registry != registry {
            // Cache file says it belongs to a different registry (corrupt or
            // relocated path). Treat as miss so the caller reprobes.
            return Ok(None);
        }
        if !capability.is_fresh() {
            return Ok(None);
        }
        Ok(Some(capability))
    }

    /// Returns `true` if the cached probe is still within TTL.
    ///
    /// Compares `now - probed_at` against `ttl_seconds`. If the wall clock has
    /// been rewound since the probe (so `probed_at` is in the future), the
    /// subtraction errors and the entry is treated as stale — forcing a
    /// reprobe rather than extending cache lifetime arbitrarily.
    pub fn is_fresh(&self) -> bool {
        // `duration_since` errors when `probed_at` is in the future (rewound
        // wall clock or corrupted cache). Treat that as stale so the caller
        // reprobes rather than trusting a bogus timestamp.
        match SystemTime::now().duration_since(self.probed_at) {
            Ok(elapsed) => elapsed < Duration::from_secs(self.ttl_seconds),
            Err(_) => false,
        }
    }
}

/// Compute the on-disk path of the capability cache for `registry` under
/// `cache_root`.
///
/// Layout: `{ocx_home}/state/referrers/{registry_slug}.json`.
/// Matches the path produced by [`ReferrersApiCapability::write_cache`] so
/// that `from_cache` and `write_cache` are always consistent.
fn cache_path(cache_root: &Path, registry: &str) -> PathBuf {
    cache_root
        .join("state")
        .join("referrers")
        .join(format!("{}.json", registry.to_relaxed_slug()))
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`ReferrersApiCapability`].
    use std::sync::{Arc, RwLock};

    use async_trait::async_trait;

    use super::*;
    use crate::oci::client::error::ClientError;
    use crate::oci::{self, RegistryOperation};

    // ── Minimal stub transport for probe tests ──────────────────────────────

    /// Configures what `list_referrers` returns on a [`ProbeStub`].
    enum ReferrersResponse {
        /// Return an empty referrer list (registry supports the API).
        Supported,
        /// Return `ClientError::ReferrersUnsupported`.
        Unsupported,
        /// Return a generic `ClientError::Registry` error.
        Error(String),
    }

    struct ProbeStubInner {
        response: ReferrersResponse,
    }

    struct ProbeStub {
        inner: Arc<RwLock<ProbeStubInner>>,
    }

    impl ProbeStub {
        fn supported() -> Self {
            Self {
                inner: Arc::new(RwLock::new(ProbeStubInner {
                    response: ReferrersResponse::Supported,
                })),
            }
        }

        fn unsupported() -> Self {
            Self {
                inner: Arc::new(RwLock::new(ProbeStubInner {
                    response: ReferrersResponse::Unsupported,
                })),
            }
        }

        fn error(msg: &str) -> Self {
            Self {
                inner: Arc::new(RwLock::new(ProbeStubInner {
                    response: ReferrersResponse::Error(msg.to_string()),
                })),
            }
        }
    }

    #[async_trait]
    impl OciTransport for ProbeStub {
        async fn ensure_auth(
            &self,
            _image: &oci::native::Reference,
            _op: RegistryOperation,
        ) -> Result<(), ClientError> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>, ClientError> {
            unimplemented!()
        }

        async fn catalog(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>, ClientError> {
            unimplemented!()
        }

        async fn fetch_manifest_digest(&self, _image: &oci::native::Reference) -> Result<String, ClientError> {
            unimplemented!()
        }

        async fn pull_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _accepted_media_types: &[&str],
        ) -> Result<(Vec<u8>, String), ClientError> {
            unimplemented!()
        }

        async fn pull_blob(&self, _image: &oci::native::Reference, _digest: &Digest) -> Result<Vec<u8>, ClientError> {
            unimplemented!()
        }

        async fn pull_blob_to_file(
            &self,
            _image: &oci::native::Reference,
            _digest: &Digest,
            _path: &std::path::Path,
            _total_size: u64,
            _on_progress: Arc<dyn Fn(u64) + Send + Sync>,
        ) -> Result<(), ClientError> {
            unimplemented!()
        }

        async fn head_blob(&self, _image: &oci::native::Reference, _digest: &Digest) -> Result<u64, ClientError> {
            unimplemented!()
        }

        async fn push_manifest(
            &self,
            _image: &oci::native::Reference,
            _manifest: &oci::Manifest,
        ) -> Result<String, ClientError> {
            unimplemented!()
        }

        async fn push_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _media_type: &str,
        ) -> Result<String, ClientError> {
            unimplemented!()
        }

        async fn push_blob(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _digest: &Digest,
            _on_progress: Arc<dyn Fn(u64) + Send + Sync>,
        ) -> Result<String, ClientError> {
            unimplemented!()
        }

        async fn push_referrer_manifest(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &Digest,
            _manifest_bytes: &[u8],
            _media_type: &str,
        ) -> Result<oci::Descriptor, ClientError> {
            unimplemented!()
        }

        async fn list_referrers(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &Digest,
        ) -> Result<Vec<oci::Descriptor>, ClientError> {
            let inner = self.inner.read().unwrap();
            match &inner.response {
                ReferrersResponse::Supported => Ok(vec![]),
                ReferrersResponse::Unsupported => Err(ClientError::ReferrersUnsupported {
                    registry: "test.example".to_string(),
                }),
                ReferrersResponse::Error(msg) => Err(ClientError::Registry(msg.clone().into())),
            }
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(ProbeStub {
                inner: self.inner.clone(),
            })
        }
    }

    fn zero_digest() -> Digest {
        Digest::Sha256("0".repeat(64))
    }

    // ── probe tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn probe_404_returns_unsupported() {
        let transport = ProbeStub::unsupported();
        let cap = ReferrersApiCapability::probe(&transport, "test.example", "myrepo", &zero_digest())
            .await
            .expect("probe must not error on 404");
        assert_eq!(cap.supported, ReferrersSupport::Unsupported);
        assert_eq!(cap.registry, "test.example");
        assert!(cap.is_fresh());
    }

    #[tokio::test]
    async fn probe_200_returns_supported() {
        let transport = ProbeStub::supported();
        let cap = ReferrersApiCapability::probe(&transport, "test.example", "myrepo", &zero_digest())
            .await
            .expect("probe must not error on 200");
        assert_eq!(cap.supported, ReferrersSupport::Supported);
        assert!(cap.is_fresh());
    }

    #[tokio::test]
    async fn probe_500_propagates_error() {
        let transport = ProbeStub::error("internal server error");
        let result = ReferrersApiCapability::probe(&transport, "test.example", "myrepo", &zero_digest()).await;
        assert!(result.is_err(), "non-404 error must propagate");
    }

    // ── write_cache + from_cache round-trip ──────────────────────────────────

    #[tokio::test]
    async fn write_and_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".to_string(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        };

        // Write via write_cache (uses the state/referrers/ layout).
        cap.write_cache(tmp.path()).await.unwrap();

        // Verify the file is valid JSON.
        let slug = "ghcr.io".to_relaxed_slug();
        let file_path = tmp.path().join("state").join("referrers").join(format!("{slug}.json"));
        let raw = std::fs::read(&file_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(parsed["registry"], "ghcr.io");
        assert_eq!(parsed["supported"], "supported");
    }

    #[tokio::test]
    async fn write_cache_file_is_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = ReferrersApiCapability {
            registry: "registry.example".to_string(),
            supported: ReferrersSupport::Unsupported,
            probed_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        };
        cap.write_cache(tmp.path()).await.unwrap();

        let slug = "registry.example".to_relaxed_slug();
        let path = tmp.path().join("state").join("referrers").join(format!("{slug}.json"));
        let bytes = std::fs::read(&path).unwrap();
        // Must parse without error.
        let _: serde_json::Value = serde_json::from_slice(&bytes).expect("written file must be valid JSON");
    }

    /// Repeated `write_cache` calls for the same registry must succeed —
    /// the second write replaces the first atomically. On Windows this
    /// fails with `NamedTempFile::persist`; the implementation must use
    /// `std::fs::rename` (replace-existing on every platform). On POSIX
    /// this regression-tests the same contract.
    #[tokio::test]
    async fn write_cache_replaces_existing_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = "ghcr.io";

        let first = ReferrersApiCapability {
            registry: registry.to_string(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        };
        first.write_cache(tmp.path()).await.expect("first write must succeed");

        let second = ReferrersApiCapability {
            registry: registry.to_string(),
            supported: ReferrersSupport::Unsupported,
            probed_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        };
        second
            .write_cache(tmp.path())
            .await
            .expect("second write must succeed (replace-existing)");

        // Reload and confirm the second value won — proves replace happened.
        let loaded = ReferrersApiCapability::from_cache(registry, tmp.path())
            .await
            .expect("from_cache must not error")
            .expect("from_cache must return Some for the replaced entry");
        assert_eq!(loaded.supported, ReferrersSupport::Unsupported);
    }

    // ── existing from_cache tests (preserved) ───────────────────────────────

    #[test]
    fn referrers_support_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReferrersSupport::Supported).unwrap(),
            "\"supported\""
        );
        assert_eq!(
            serde_json::to_string(&ReferrersSupport::Unsupported).unwrap(),
            "\"unsupported\"",
        );
    }

    #[test]
    fn referrers_support_deserializes_snake_case() {
        let supported: ReferrersSupport = serde_json::from_str("\"supported\"").unwrap();
        assert_eq!(supported, ReferrersSupport::Supported);
        let unsupported: ReferrersSupport = serde_json::from_str("\"unsupported\"").unwrap();
        assert_eq!(unsupported, ReferrersSupport::Unsupported);
    }

    #[test]
    fn referrers_support_rejects_unknown_variant() {
        // Typo/unknown values must fail — no silent Default fallback.
        let result: Result<ReferrersSupport, _> = serde_json::from_str("\"probing\"");
        assert!(result.is_err(), "unknown variant should be rejected");
    }

    #[test]
    fn is_fresh_returns_false_when_expired() {
        // probed_at=epoch, ttl=1h: now (2026) is far past — not fresh.
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::UNIX_EPOCH,
            ttl_seconds: 3600,
        };
        assert!(!cap.is_fresh(), "ancient probe must not be fresh");
    }

    #[test]
    fn is_fresh_returns_true_when_within_ttl() {
        // Probed 10 seconds ago with a 1-hour TTL: fresh.
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now() - Duration::from_secs(10),
            ttl_seconds: 3600,
        };
        assert!(cap.is_fresh(), "recent probe must be fresh");
    }

    #[test]
    fn is_fresh_future_probed_at_treated_as_stale() {
        // If the wall clock jumped backwards after probing, `probed_at` is in
        // the future. Treat the cache as stale so the caller reprobes rather
        // than trusting a timestamp that cannot be reconciled with `now`.
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now() + Duration::from_secs(3600),
            ttl_seconds: 3600,
        };
        assert!(!cap.is_fresh(), "future-dated probe must be stale (forces reprobe)");
    }

    #[tokio::test]
    async fn from_cache_returns_none_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = ReferrersApiCapability::from_cache("ghcr.io", tmp.path()).await.unwrap();
        assert!(result.is_none(), "missing file → None (not error)");
    }

    #[tokio::test]
    async fn from_cache_returns_none_when_file_corrupt() {
        // Fail-open: corrupt JSON returns None, not Err.
        // Write the corrupt file at the canonical path (state/referrers/<slug>.json).
        let tmp = tempfile::tempdir().unwrap();
        let slug = "ghcr.io".to_relaxed_slug();
        let dir = tmp.path().join("state").join("referrers");
        let path = dir.join(format!("{slug}.json"));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(&path, b"not json").await.unwrap();
        let result = ReferrersApiCapability::from_cache("ghcr.io", tmp.path()).await.unwrap();
        assert!(result.is_none(), "corrupt file → None (fail-open)");
    }

    #[tokio::test]
    async fn from_cache_returns_none_when_expired() {
        let tmp = tempfile::tempdir().unwrap();
        let slug = "ghcr.io".to_relaxed_slug();
        let dir = tmp.path().join("state").join("referrers");
        let path = dir.join(format!("{slug}.json"));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::UNIX_EPOCH,
            ttl_seconds: 1,
        };
        tokio::fs::write(&path, serde_json::to_vec(&cap).unwrap())
            .await
            .unwrap();
        let result = ReferrersApiCapability::from_cache("ghcr.io", tmp.path()).await.unwrap();
        assert!(result.is_none(), "expired cache → None");
    }

    #[tokio::test]
    async fn from_cache_returns_some_when_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let slug = "ghcr.io".to_relaxed_slug();
        let dir = tmp.path().join("state").join("referrers");
        let path = dir.join(format!("{slug}.json"));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now(),
            ttl_seconds: 3600,
        };
        tokio::fs::write(&path, serde_json::to_vec(&cap).unwrap())
            .await
            .unwrap();
        let result = ReferrersApiCapability::from_cache("ghcr.io", tmp.path())
            .await
            .unwrap()
            .expect("fresh cache returns Some");
        assert_eq!(result.registry, "ghcr.io");
        assert_eq!(result.supported, ReferrersSupport::Supported);
    }

    #[tokio::test]
    async fn from_cache_returns_none_when_registry_mismatch() {
        // Cache file on disk says registry=a.example, caller asks for b.example.
        // Corrupt-relocated; treat as miss so caller reprobes.
        // Write data for "a.example" at the b.example slug path to simulate the mismatch.
        let tmp = tempfile::tempdir().unwrap();
        let slug = "b.example".to_relaxed_slug();
        let dir = tmp.path().join("state").join("referrers");
        let path = dir.join(format!("{slug}.json"));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let cap = ReferrersApiCapability {
            registry: "a.example".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now(),
            ttl_seconds: 3600,
        };
        tokio::fs::write(&path, serde_json::to_vec(&cap).unwrap())
            .await
            .unwrap();
        let result = ReferrersApiCapability::from_cache("b.example", tmp.path())
            .await
            .unwrap();
        assert!(result.is_none(), "registry mismatch → None");
    }

    /// `write_cache` must never write outside `cache_root`, even when the
    /// registry string contains path-traversal sequences like `../evil`,
    /// `/etc/passwd`, or `..`. The `to_relaxed_slug()` helper replaces `/`
    /// and `.` (when forming `..`) with `_`, so these become harmless slugs.
    #[tokio::test]
    async fn cache_path_rejects_hostile_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path();

        let hostile_cases = ["../evil", "/etc/passwd", "..", "../../etc/shadow", "foo/../bar"];

        for registry in hostile_cases {
            let cap = ReferrersApiCapability {
                registry: registry.to_string(),
                supported: ReferrersSupport::Supported,
                probed_at: SystemTime::now(),
                ttl_seconds: TTL_SECS,
            };
            cap.write_cache(cache_root)
                .await
                .unwrap_or_else(|e| panic!("write_cache failed for {registry:?}: {e}"));

            // Verify every file written is strictly under cache_root.
            // Walk referrers_capability/ and assert no escape.
            let cap_dir = cache_root.join("state").join("referrers");
            for entry in std::fs::read_dir(&cap_dir).expect("read cap dir") {
                let entry = entry.unwrap();
                let file_path = entry.path().canonicalize().unwrap_or_else(|_| entry.path());
                let root_canonical = cache_root.canonicalize().unwrap_or_else(|_| cache_root.to_path_buf());
                assert!(
                    file_path.starts_with(&root_canonical),
                    "file {file_path:?} escapes cache_root {root_canonical:?} for registry {registry:?}"
                );
            }
        }
    }

    /// End-to-end round-trip: `write_cache` followed by `from_cache` must
    /// return a value equal to what was written. This locks the path invariant
    /// — if the two methods ever diverge again, this test fails.
    #[tokio::test]
    async fn write_cache_then_from_cache_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".to_string(),
            supported: ReferrersSupport::Unsupported,
            probed_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        };

        cap.write_cache(tmp.path()).await.expect("write_cache must succeed");

        let loaded = ReferrersApiCapability::from_cache("ghcr.io", tmp.path())
            .await
            .expect("from_cache must not error")
            .expect("from_cache must return Some for a just-written entry");

        assert_eq!(loaded.registry, cap.registry);
        assert_eq!(loaded.supported, cap.supported);
        assert_eq!(loaded.ttl_seconds, cap.ttl_seconds);
    }
}
