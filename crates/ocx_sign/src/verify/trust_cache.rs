// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Trust material captured by an online verify, reused by a later offline one
//! (`adr_offline_verify_trust_cache.md` § Trust-root cache).

use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use url::Url;

use super::trust_root::TrustRoot;
use crate::sign::state::SigningStatePaths;

/// Offline freshness bound; TUF metadata expiry is a separate limit, enforced only online.
const TTL_SECS: u64 = 24 * 3600;

/// Cached Sigstore trust material for one Rekor instance; fail-open, so a corrupt or mismatched file is a miss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRootCache {
    /// Rekor URL authority (host[:port]) this material belongs to.
    pub rekor_authority: String,

    /// Fulcio CA certificate(s), DER-encoded.
    pub fulcio_der_certs: Vec<Vec<u8>>,

    /// CTFE log keys, `logId` hex -> DER SPKI.
    // No `serde(default)`: an entry without CTFE keys cannot check the SCT, so it must fail to parse and refetch.
    pub ctfe_keys: BTreeMap<String, Vec<u8>>,

    /// Rekor public key (PEM) used to verify the Signed Entry Timestamp; `Some` in every written entry.
    pub rekor_public_key_pem: Option<String>,

    /// Wall-clock time the material was cached (UTC).
    pub cached_at: SystemTime,

    /// TTL in seconds, clamped to `TTL_SECS` on read.
    pub ttl_seconds: u64,
}

impl TrustRootCache {
    /// Build a cache record from the trust material of a successful online verify.
    pub fn new(
        rekor_authority: String,
        fulcio_der_certs: Vec<Vec<u8>>,
        ctfe_keys: BTreeMap<String, Vec<u8>>,
        rekor_public_key_pem: String,
    ) -> Self {
        Self {
            rekor_authority,
            fulcio_der_certs,
            ctfe_keys,
            rekor_public_key_pem: Some(rekor_public_key_pem),
            cached_at: SystemTime::now(),
            ttl_seconds: TTL_SECS,
        }
    }

    /// Persist the record atomically to [`SigningStatePaths::trust_root_file`].
    pub async fn write_cache(&self, state: &SigningStatePaths) -> io::Result<()> {
        let target = state.trust_root_file(&self.rekor_authority);
        let dir = target
            .parent()
            .ok_or_else(|| io::Error::other("cache path has no parent"))?
            .to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;

        let bytes = serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        tokio::task::spawn_blocking(move || ocx_util::fs::write_bytes_atomic(&target, &bytes))
            .await
            .map_err(|e| io::Error::other(format!("trust-root cache tempfile+rename panicked: {e}")))??;
        Ok(())
    }

    /// Read a cached entry for `rekor_authority` without any network.
    ///
    /// `Ok(None)` when the file is missing, expired, corrupt, or belongs to another authority.
    pub async fn from_cache(rekor_authority: &str, state: &SigningStatePaths) -> io::Result<Option<Self>> {
        let path = state.trust_root_file(rekor_authority);
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

    /// Returns `true` if the entry is within TTL; a `cached_at` in the future is stale.
    pub fn is_fresh(&self) -> bool {
        // Clamp: the file's own `ttl_seconds` of `u64::MAX` would otherwise pin its trust anchors forever.
        match SystemTime::now().duration_since(self.cached_at) {
            Ok(elapsed) => elapsed < Duration::from_secs(self.ttl_seconds.min(TTL_SECS)),
            Err(_) => false,
        }
    }

    /// Build a [`TrustRoot`] from the cached material.
    ///
    /// Offline callers must check [`TrustRoot::rekor_public_key_pem`] is `Some`: without it the SET cannot verify.
    pub fn into_trust_root(self) -> TrustRoot {
        let root = TrustRoot::from_material(self.fulcio_der_certs, self.ctfe_keys, BTreeMap::new());
        match self.rekor_public_key_pem {
            // An unparseable PEM yields the keyless root, so cache damage refetches instead of failing the verify.
            Some(pem) => root.clone().with_rekor_key_pem(&pem).unwrap_or(root),
            None => root,
        }
    }
}

/// The trust-root cache key for a Rekor URL: its authority (`host[:port]`), or the whole URL when host-less.
///
/// The pipeline writes and the CLI reads under this key, so both must derive it here.
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

    fn ctfe() -> BTreeMap<String, Vec<u8>> {
        // One throwaway CT log key; the cache does not validate it either.
        BTreeMap::from([("c0ffee".to_string(), vec![0x30, 0x00])])
    }

    fn state_in(tmp: &tempfile::TempDir) -> SigningStatePaths {
        SigningStatePaths::new(tmp.path().join("state"))
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let entry = TrustRootCache::new("rekor.example:443".into(), certs(), ctfe(), "PEMKEY".into());
        entry.write_cache(&state).await.expect("write");

        let loaded = TrustRootCache::from_cache("rekor.example:443", &state)
            .await
            .expect("read")
            .expect("fresh entry present");
        assert_eq!(loaded.rekor_authority, "rekor.example:443");
        assert_eq!(loaded.fulcio_der_certs, certs());
        assert_eq!(loaded.rekor_public_key_pem.as_deref(), Some("PEMKEY"));
    }

    fn key_pem() -> String {
        // Body is opaque to the trust root; only the PEM envelope must parse.
        pem::encode(&pem::Pem::new("PUBLIC KEY", vec![0x30, 0x03, 0x02, 0x01, 0x07]))
    }

    #[tokio::test]
    async fn into_trust_root_carries_pinned_rekor_key() {
        let entry = TrustRootCache::new("rekor.example".into(), certs(), ctfe(), key_pem());
        let root = entry.into_trust_root();
        assert_eq!(root.der_certs().len(), 2);
        assert_eq!(
            root.ctfe_key_map(),
            &ctfe(),
            "CT log keys must survive the cache round-trip"
        );
        let recovered = root.rekor_public_key_pem().expect("pinned Rekor key");
        assert_eq!(
            pem::parse(&recovered).unwrap().contents(),
            pem::parse(key_pem()).unwrap().contents(),
            "the pinned key must round-trip byte-for-byte through DER"
        );
    }

    /// A cached Rekor key that is not PEM is dropped rather than propagated:
    /// the cache is fail-open everywhere else, and offline verify then hits the
    /// actionable "no pinned Rekor key" remedy instead of a parse error from a
    /// file the user never wrote by hand.
    #[tokio::test]
    async fn malformed_cached_rekor_key_degrades_to_no_pinned_key() {
        let entry = TrustRootCache::new("rekor.example".into(), certs(), ctfe(), "NOT A PEM".into());
        let root = entry.into_trust_root();
        assert!(
            root.rekor_public_key_pem().is_none(),
            "a malformed PEM must not pin a key"
        );
        assert_eq!(root.der_certs().len(), 2, "the anchors still survive");
    }

    #[tokio::test]
    async fn missing_file_is_a_miss_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let result = TrustRootCache::from_cache("absent.example", &state).await.unwrap();
        assert!(result.is_none());
    }

    /// The `ctfe_keys` field carries no `serde(default)` on purpose: an entry
    /// written before it existed must deserialize-fail, and the fail-open reader
    /// turns that into a miss + refetch. Without this, a pre-field entry would
    /// load with an empty CT-key map and every SCT check off it would fail.
    #[tokio::test]
    async fn entry_without_ctfe_keys_is_a_miss_not_an_empty_map() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let mut json = serde_json::to_value(TrustRootCache::new(
            "rekor.example".into(),
            certs(),
            ctfe(),
            "PEM".into(),
        ))
        .unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("ctfe_keys")
            .expect("field present before removal");
        let path = state.trust_root_file("rekor.example");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, serde_json::to_vec(&json).unwrap())
            .await
            .unwrap();

        let result = TrustRootCache::from_cache("rekor.example", &state).await.unwrap();
        assert!(result.is_none(), "an entry predating ctfe_keys must be a miss");
    }

    #[tokio::test]
    async fn corrupt_file_fails_open_to_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let path = state.trust_root_file("rekor.example");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, b"not json").await.unwrap();
        let result = TrustRootCache::from_cache("rekor.example", &state).await.unwrap();
        assert!(result.is_none(), "corrupt cache must fail open to a miss");
    }

    #[tokio::test]
    async fn expired_entry_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let entry = TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: certs(),
            ctfe_keys: ctfe(),
            rekor_public_key_pem: Some("PEM".into()),
            cached_at: SystemTime::UNIX_EPOCH,
            ttl_seconds: 1,
        };
        entry.write_cache(&state).await.unwrap();
        let result = TrustRootCache::from_cache("rekor.example", &state).await.unwrap();
        assert!(result.is_none(), "expired entry must be a miss");
    }

    #[tokio::test]
    async fn authority_mismatch_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        // Write an entry that claims a different authority than its filename slug.
        let path = state.trust_root_file("b.example");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        let entry = TrustRootCache::new("a.example".into(), certs(), ctfe(), "PEM".into());
        tokio::fs::write(&path, serde_json::to_vec(&entry).unwrap())
            .await
            .unwrap();
        let result = TrustRootCache::from_cache("b.example", &state).await.unwrap();
        assert!(result.is_none(), "authority mismatch must be a miss");
    }

    #[tokio::test]
    async fn hostile_authority_stays_under_cache_root() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        for authority in ["../evil", "/etc/passwd", "..", "a/../b"] {
            let entry = TrustRootCache::new(authority.into(), certs(), ctfe(), "PEM".into());
            entry.write_cache(&state).await.unwrap();
        }
        let dir = state.trust_root_file("x").parent().unwrap().to_path_buf();
        for file in std::fs::read_dir(&dir).unwrap() {
            let path = file.unwrap().path().canonicalize().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            assert!(path.starts_with(&root), "{path:?} escaped {root:?}");
        }
    }

    #[test]
    fn a_file_declared_ttl_cannot_outlive_the_built_in_ceiling() {
        // The lifetime is attacker-controllable in the one scenario that
        // matters: a single write into the state dir (a restored CI cache, a
        // shared OCX_HOME) plants trust material and names its own expiry.
        let entry = TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: certs(),
            ctfe_keys: ctfe(),
            rekor_public_key_pem: Some("PEM".into()),
            cached_at: SystemTime::now() - Duration::from_secs(TTL_SECS + 60),
            ttl_seconds: u64::MAX,
        };
        assert!(
            !entry.is_fresh(),
            "an entry past TTL_SECS must be stale however long the file claims to live"
        );
    }

    #[test]
    fn a_file_declared_ttl_below_the_ceiling_still_shortens_the_lifetime() {
        let entry = TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: certs(),
            ctfe_keys: ctfe(),
            rekor_public_key_pem: Some("PEM".into()),
            cached_at: SystemTime::now() - Duration::from_secs(120),
            ttl_seconds: 60,
        };
        assert!(!entry.is_fresh(), "clamping must not round a short TTL up");
    }

    #[test]
    fn future_cached_at_is_stale() {
        let entry = TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: certs(),
            ctfe_keys: ctfe(),
            rekor_public_key_pem: Some("PEM".into()),
            cached_at: SystemTime::now() + Duration::from_secs(3600),
            ttl_seconds: 3600,
        };
        assert!(!entry.is_fresh(), "future-dated entry must be stale");
    }
}
