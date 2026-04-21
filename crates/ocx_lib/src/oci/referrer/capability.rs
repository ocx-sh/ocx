// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Capability probe + cache for the OCI Referrers API.
//!
//! A registry either supports the Referrers API (`/v2/<name>/referrers/<digest>`)
//! or it does not. OCX probes the endpoint once and caches the result per
//! registry under `~/.ocx/blobs/{registry}/.capabilities.json`. The cache is
//! consulted before each referrer operation; TTL is 24h in CI and 1h
//! interactively.
//!
//! Phase 1 stub — shape only. The probe and cache logic are implemented in
//! Phase 5. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! §"Capability cache".

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::oci::client::Client;

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
/// Stored at `~/.ocx/blobs/{registry}/.capabilities.json`. The cache is
/// advisory and fail-open: a corrupt file is treated as a cache miss.
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
    /// Probe the registry's Referrers API support and persist the result.
    ///
    /// Performs a single `GET /v2/<name>/referrers/<zero-digest>` request
    /// against the registry. 200 → [`ReferrersSupport::Supported`];
    /// 404/405 → [`ReferrersSupport::Unsupported`]. Other statuses propagate
    /// as a [`ClientError`](crate::oci::client::ClientError).
    ///
    /// Writes the result atomically (temp-file + rename) to
    /// `~/.ocx/blobs/{registry}/.capabilities.json` unless `no_cache` is true.
    ///
    /// Phase 5c TODO: the actual OCI client integration. The shape is locked
    /// in (Client + registry + cache_root + no_cache) so downstream code can
    /// wire the call site without blocking on crypto integration.
    pub async fn probe(
        _client: &Client,
        _registry: &str,
        _cache_root: &Path,
        _no_cache: bool,
    ) -> Result<Self, crate::oci::client::error::ClientError> {
        unimplemented!(
            "ReferrersApiCapability::probe — Phase 5c wires the OCI client \
             referrers GET and the atomic write-through"
        )
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
    pub async fn from_cache(registry: &str, cache_root: &Path) -> std::io::Result<Option<Self>> {
        let path = cache_path(cache_root, registry);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
    /// Compares `probed_at + ttl_seconds` against the current wall clock;
    /// clock going backwards counts as "not fresh" so a rewound clock forces
    /// a reprobe rather than extending cache lifetime arbitrarily.
    pub fn is_fresh(&self) -> bool {
        let expiry = self.probed_at + Duration::from_secs(self.ttl_seconds);
        // duration_since returns Err when `expiry` is in the future — i.e. the
        // cache entry is still valid. A rewound wall clock produces the same
        // branch, which forces a reprobe rather than extending TTL arbitrarily.
        SystemTime::now().duration_since(expiry).is_err()
    }
}

/// Compute the on-disk path of the capability cache for `registry` under
/// `cache_root`.
///
/// Layout matches the blob CAS convention (`blobs/{registry}/...`) with a
/// dotfile name so normal CAS walkers ignore it.
fn cache_path(cache_root: &Path, registry: &str) -> PathBuf {
    cache_root.join(registry).join(".capabilities.json")
}

#[cfg(test)]
mod tests {
    //! Serde shape contract for [`ReferrersSupport`].
    //!
    //! The serialized form appears in the on-disk capabilities cache AND in the
    //! JSON error envelope's `context.referrers_supported` field, so the wire
    //! format is part of the public contract.
    use super::*;

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
    fn is_fresh_handles_probed_at_in_future() {
        // If wall clock jumped backwards after probing, `probed_at` is in the
        // future. We deliberately treat the cache as still fresh (conservative:
        // avoids invalidating cache on every clock skew).
        let cap = ReferrersApiCapability {
            registry: "ghcr.io".into(),
            supported: ReferrersSupport::Supported,
            probed_at: SystemTime::now() + Duration::from_secs(3600),
            ttl_seconds: 3600,
        };
        assert!(cap.is_fresh(), "future-dated probe counts as fresh");
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
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ghcr.io").join(".capabilities.json");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&path, b"not json").await.unwrap();
        let result = ReferrersApiCapability::from_cache("ghcr.io", tmp.path()).await.unwrap();
        assert!(result.is_none(), "corrupt file → None (fail-open)");
    }

    #[tokio::test]
    async fn from_cache_returns_none_when_expired() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ghcr.io").join(".capabilities.json");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
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
        let path = tmp.path().join("ghcr.io").join(".capabilities.json");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
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
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("b.example").join(".capabilities.json");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
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
}
