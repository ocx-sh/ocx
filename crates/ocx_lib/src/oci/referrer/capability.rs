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

use std::path::Path;
use std::time::SystemTime;

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
    pub async fn probe(
        _client: &Client,
        _registry: &str,
        _cache_root: &Path,
        _no_cache: bool,
    ) -> Result<Self, crate::oci::client::error::ClientError> {
        unimplemented!("ReferrersApiCapability::probe — Phase 5 implementation")
    }

    /// Read a cached capability from disk without probing.
    ///
    /// Returns `Ok(None)` when the cache file is missing, expired, or
    /// corrupt (fail-open). Returns `Ok(Some(_))` when a fresh entry is
    /// available.
    pub async fn from_cache(_registry: &str, _cache_root: &Path) -> std::io::Result<Option<Self>> {
        unimplemented!("ReferrersApiCapability::from_cache — Phase 5 implementation")
    }

    /// Returns `true` if the cached probe is still within TTL.
    pub fn is_fresh(&self) -> bool {
        unimplemented!("ReferrersApiCapability::is_fresh — Phase 5 implementation")
    }
}
