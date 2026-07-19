// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch publish` — publish a `__ocx.patch` descriptor to the patch registry.
//!
//! This module implements Phase 6A of the infrastructure-patches feature
//! (`adr_infrastructure_patches.md`, milestone #111, issue #117).
//!
//! ## Responsibility
//!
//! [`PackageManager::publish_patch_descriptor`] validates an authored descriptor,
//! requires an online client, and pushes the descriptor manifest to the patch
//! registry under the `__ocx.patch` internal tag of `patch_repo_id`. The CLI
//! computes `patch_repo_id` (global root vs package-specific sub-path) via the
//! discovery helpers in [`super::patch_discovery`]; this method stays
//! identifier-agnostic.
//!
//! ## Companions are out of scope
//!
//! A patch descriptor only *references* companion packages by identifier. The
//! maintainer publishes companions separately with `ocx package push`. This
//! method pushes the descriptor manifest only.

use crate::{
    oci::{self, Identifier},
    patch::PatchDescriptor,
};

use super::super::{PackageManager, error::PackageErrorKind};

// ── Public report type ────────────────────────────────────────────────────────

/// Summary of a completed [`PackageManager::publish_patch_descriptor`] run.
///
/// Carries the published patch repo reference, the manifest digest of the
/// pushed `__ocx.patch` artifact, and the descriptor's rule count.
#[derive(Debug, Clone)]
pub struct PatchPublishReport {
    /// Canonical reference of the patch repo the descriptor was published to
    /// (`registry/repository:__ocx.patch`).
    pub patch_reference: String,
    /// Manifest digest of the pushed `__ocx.patch` artifact.
    pub manifest_digest: oci::Digest,
    /// Number of rules in the published descriptor.
    pub rule_count: usize,
}

// ── PackageManager::publish_patch_descriptor ──────────────────────────────────

impl PackageManager {
    /// Publish a validated patch descriptor to `patch_repo_id`'s `__ocx.patch` tag.
    ///
    /// Steps:
    ///
    /// 1. Parse and validate `descriptor_bytes` as a [`PatchDescriptor`]
    ///    (rejects malformed input before any network call).
    /// 2. Require an online client (`require_client`; offline → `OfflineMode`).
    /// 3. Push the descriptor manifest via `Client::push_patch_descriptor`.
    /// 4. Return the published reference, manifest digest, and rule count.
    ///
    /// `patch_repo_id` is the patch-registry repository (global root or
    /// package-specific sub-path) WITHOUT the `__ocx.patch` tag — the client
    /// applies the internal tag.
    ///
    /// # Errors
    ///
    /// - `PackageErrorKind::PatchDiscovery` — the descriptor bytes are not a
    ///   valid patch descriptor.
    /// - `PackageErrorKind::Internal` — offline mode, or a registry push error.
    pub async fn publish_patch_descriptor(
        &self,
        patch_repo_id: &Identifier,
        descriptor_bytes: &[u8],
    ) -> Result<PatchPublishReport, PackageErrorKind> {
        // Step 1: Validate the descriptor parses; capture the rule count for the report.
        let descriptor =
            PatchDescriptor::from_json_bytes(descriptor_bytes).map_err(PackageErrorKind::PatchDiscovery)?;
        let rule_count = descriptor.rules.len();

        // Step 2: Require an online client.
        let client = self.require_client().map_err(PackageErrorKind::Internal)?;

        // Step 3: Push the descriptor manifest.
        let manifest_digest = client
            .push_patch_descriptor(patch_repo_id, descriptor_bytes)
            .await
            .map_err(|e| PackageErrorKind::Internal(e.into()))?;

        // Step 4: Build the report.
        let patch_reference = patch_repo_id
            .clone_with_tag(crate::package::tag::InternalTag::PATCH_TAG)
            .to_string();

        Ok(PatchPublishReport {
            patch_reference,
            manifest_digest,
            rule_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::{
        file_structure::FileStructure,
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
    };

    fn make_offline_manager(ocx_home: &Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            snapshot_store: fs.index.clone(),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000")
    }

    fn valid_descriptor_bytes() -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": ["internal.company.com/certs/ca:latest"] }]
        })
        .to_string()
        .into_bytes()
    }

    /// Offline publish surfaces `OfflineMode` (mapped to `Internal`) — no panic.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_offline_returns_error() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path());
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let result = manager
            .publish_patch_descriptor(&patch_repo, &valid_descriptor_bytes())
            .await;
        assert!(result.is_err(), "offline publish must return Err");
        let debug = format!("{:?}", result.unwrap_err());
        assert!(
            debug.contains("OfflineMode") || debug.contains("offline") || debug.contains("Offline"),
            "offline publish error must be OfflineMode; got: {debug}"
        );
    }

    /// A malformed descriptor is rejected before any client requirement.
    #[tokio::test(flavor = "multi_thread")]
    async fn publish_rejects_malformed_descriptor() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path());
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let result = manager.publish_patch_descriptor(&patch_repo, b"not json {{{").await;
        assert!(
            matches!(result, Err(PackageErrorKind::PatchDiscovery(_))),
            "malformed descriptor must yield PatchDiscovery error, got: {result:?}"
        );
    }
}
