// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Trust-root cache for offline / air-gapped verify.
//!
//! A successful **online** `ocx package verify` captures the trust MATERIAL it
//! used — the Fulcio CA certificate(s) and the Rekor public key — so a later
//! **offline** verify can reuse it without contacting the Sigstore trust
//! services. The cache mirrors the shape of the referrers capability cache
//! (`oci/referrer/capability.rs`): an atomic tempfile+rename write, a TTL-gated
//! fail-open read, and a host-scoped key.
//!
//! Layout: `{ocx_home}/state/trust_root/{rekor_authority_slug}.json`. Keyed by
//! the Rekor URL authority so public and private Sigstore instances never
//! collide; the cache is per-`OCX_HOME`.
//!
//! See [`adr_offline_verify_trust_cache.md`](../../../../../.claude/artifacts/adr_offline_verify_trust_cache.md).

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use url::Url;

use super::trust_root::TrustRoot;
use crate::prelude::StringExt;

/// Cache TTL: 24 hours.
///
/// Trust roots rotate on the order of weeks; 24h bounds how stale offline
/// material may be while still surviving a "verified yesterday, on a plane
/// today" gap. Honoring the real TUF metadata expiry is deferred with the real
/// TUF client — until then this fixed ceiling is the freshness bound.
const TTL_SECS: u64 = 24 * 3600;

/// Cached Sigstore trust material for one Rekor instance.
///
/// Stored at `{ocx_home}/state/trust_root/{rekor_authority_slug}.json`. The
/// cache is advisory and fail-open: a corrupt or mismatched file is treated as
/// a miss, so a bad cache never turns into a verification failure — the caller
/// falls back to an online fetch (or, offline, to an actionable error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRootCache {
    /// Rekor URL authority (host[:port]) this material belongs to.
    pub rekor_authority: String,

    /// Fulcio CA certificate(s), DER-encoded — the chain anchors the online
    /// verify validated against.
    pub fulcio_der_certs: Vec<Vec<u8>>,

    /// Rekor public key (PEM) used to verify the Signed Entry Timestamp.
    ///
    /// Always `Some` for a written entry (offline verify needs it); `Option`
    /// only so a hand-authored/partial file degrades to a miss rather than a
    /// deserialize error.
    pub rekor_public_key_pem: Option<String>,

    /// Wall-clock time the material was cached (UTC).
    pub cached_at: SystemTime,

    /// TTL in seconds; the reader compares `cached_at + ttl` against now.
    pub ttl_seconds: u64,
}

impl TrustRootCache {
    /// Build a cache record from the trust material of a successful online verify.
    pub fn new(rekor_authority: String, fulcio_der_certs: Vec<Vec<u8>>, rekor_public_key_pem: String) -> Self {
        Self {
            rekor_authority,
            fulcio_der_certs,
            rekor_public_key_pem: Some(rekor_public_key_pem),
            cached_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        }
    }

    /// Persist the record atomically to
    /// `{cache_root}/state/trust_root/{rekor_authority_slug}.json`.
    ///
    /// Tempfile + rename (replace-existing on every platform) so a concurrent
    /// reader never sees a partially-written file — identical to the referrers
    /// capability cache write.
    pub async fn write_cache(&self, cache_root: &Path) -> io::Result<()> {
        let target = cache_path(cache_root, &self.rekor_authority);
        let dir = target
            .parent()
            .ok_or_else(|| io::Error::other("cache path has no parent"))?
            .to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;

        let bytes = serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

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
            let tmp_path = tmp.into_temp_path().keep().map_err(io::Error::other)?;
            std::fs::rename(&tmp_path, &target)?;
            Ok(())
        })
        .await
        .map_err(|e| io::Error::other(format!("trust-root cache tempfile+rename panicked: {e}")))??;
        Ok(())
    }

    /// Read a cached entry for `rekor_authority` without any network.
    ///
    /// Returns `Ok(None)` when the file is missing, expired, corrupt, or belongs
    /// to a different authority (fail-open). Returns `Ok(Some(_))` for a fresh,
    /// matching entry.
    pub async fn from_cache(rekor_authority: &str, cache_root: &Path) -> io::Result<Option<Self>> {
        let path = cache_path(cache_root, rekor_authority);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let cached: Self = match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        if cached.rekor_authority != rekor_authority {
            return Ok(None);
        }
        if !cached.is_fresh() {
            return Ok(None);
        }
        Ok(Some(cached))
    }

    /// Returns `true` if the entry is within TTL.
    ///
    /// A rewound wall clock (`cached_at` in the future) is treated as stale so a
    /// bogus timestamp never extends cache lifetime.
    pub fn is_fresh(&self) -> bool {
        match SystemTime::now().duration_since(self.cached_at) {
            Ok(elapsed) => elapsed < Duration::from_secs(self.ttl_seconds),
            Err(_) => false,
        }
    }

    /// Build a [`TrustRoot`] from the cached material.
    ///
    /// The result carries the pinned Rekor key, so a verify driven by it needs
    /// no Sigstore-services network. Callers that require offline verification
    /// must check [`TrustRoot::rekor_public_key_pem`] is `Some` before relying on
    /// it — a partial/legacy cache entry without a key cannot verify the SET
    /// offline.
    pub fn into_trust_root(self) -> TrustRoot {
        TrustRoot::from_der_and_rekor(self.fulcio_der_certs, self.rekor_public_key_pem)
    }
}

/// Compute the on-disk cache path for a Rekor authority under `cache_root`.
///
/// Layout: `{cache_root}/state/trust_root/{rekor_authority_slug}.json`. The
/// relaxed slug neutralises `/`, `:`, and other path-hostile characters so a
/// hostile authority string cannot escape `cache_root`.
fn cache_path(cache_root: &Path, rekor_authority: &str) -> PathBuf {
    cache_root
        .join("state")
        .join("trust_root")
        .join(format!("{}.json", rekor_authority.to_relaxed_slug()))
}

/// The trust-root cache key for a Rekor URL: its authority (`host[:port]`).
///
/// Single source of truth so the pipeline (which writes the cache after a
/// successful online verify) and the CLI (which reads it) always agree. Falls
/// back to the whole URL string for a host-less URL so distinct instances still
/// get distinct keys.
pub fn cache_key_for_rekor(rekor_url: &Url) -> String {
    match rekor_url.host_str() {
        Some(host) => match rekor_url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        },
        None => rekor_url.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certs() -> Vec<Vec<u8>> {
        // Two throwaway DER-shaped blobs; the cache does not validate them.
        vec![vec![0x30, 0x03, 0x02, 0x01, 0x01], vec![0x30, 0x00]]
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = TrustRootCache::new("rekor.example:443".into(), certs(), "PEMKEY".into());
        entry.write_cache(tmp.path()).await.expect("write");

        let loaded = TrustRootCache::from_cache("rekor.example:443", tmp.path())
            .await
            .expect("read")
            .expect("fresh entry present");
        assert_eq!(loaded.rekor_authority, "rekor.example:443");
        assert_eq!(loaded.fulcio_der_certs, certs());
        assert_eq!(loaded.rekor_public_key_pem.as_deref(), Some("PEMKEY"));
    }

    #[tokio::test]
    async fn into_trust_root_carries_pinned_rekor_key() {
        let entry = TrustRootCache::new("rekor.example".into(), certs(), "PINNED".into());
        let root = entry.into_trust_root();
        assert_eq!(root.der_certs().len(), 2);
        assert_eq!(root.rekor_public_key_pem(), Some("PINNED"));
    }

    #[tokio::test]
    async fn missing_file_is_a_miss_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let result = TrustRootCache::from_cache("absent.example", tmp.path()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn corrupt_file_fails_open_to_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state").join("trust_root");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(format!("{}.json", "rekor.example".to_relaxed_slug()));
        tokio::fs::write(&path, b"not json").await.unwrap();
        let result = TrustRootCache::from_cache("rekor.example", tmp.path()).await.unwrap();
        assert!(result.is_none(), "corrupt cache must fail open to a miss");
    }

    #[tokio::test]
    async fn expired_entry_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: certs(),
            rekor_public_key_pem: Some("PEM".into()),
            cached_at: SystemTime::UNIX_EPOCH,
            ttl_seconds: 1,
        };
        entry.write_cache(tmp.path()).await.unwrap();
        let result = TrustRootCache::from_cache("rekor.example", tmp.path()).await.unwrap();
        assert!(result.is_none(), "expired entry must be a miss");
    }

    #[tokio::test]
    async fn authority_mismatch_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        // Write an entry that claims a different authority than its filename slug.
        let dir = tmp.path().join("state").join("trust_root");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join(format!("{}.json", "b.example".to_relaxed_slug()));
        let entry = TrustRootCache::new("a.example".into(), certs(), "PEM".into());
        tokio::fs::write(&path, serde_json::to_vec(&entry).unwrap())
            .await
            .unwrap();
        let result = TrustRootCache::from_cache("b.example", tmp.path()).await.unwrap();
        assert!(result.is_none(), "authority mismatch must be a miss");
    }

    #[tokio::test]
    async fn hostile_authority_stays_under_cache_root() {
        let tmp = tempfile::tempdir().unwrap();
        for authority in ["../evil", "/etc/passwd", "..", "a/../b"] {
            let entry = TrustRootCache::new(authority.into(), certs(), "PEM".into());
            entry.write_cache(tmp.path()).await.unwrap();
        }
        let dir = tmp.path().join("state").join("trust_root");
        for file in std::fs::read_dir(&dir).unwrap() {
            let path = file.unwrap().path().canonicalize().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            assert!(path.starts_with(&root), "{path:?} escaped {root:?}");
        }
    }

    #[test]
    fn future_cached_at_is_stale() {
        let entry = TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: certs(),
            rekor_public_key_pem: Some("PEM".into()),
            cached_at: SystemTime::now() + Duration::from_secs(3600),
            ttl_seconds: 3600,
        };
        assert!(!entry.is_fresh(), "future-dated entry must be stale");
    }
}
