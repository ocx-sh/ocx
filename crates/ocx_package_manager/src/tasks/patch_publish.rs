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

use crate::patch::PatchDescriptor;
use ocx_oci::{
    self, Algorithm, Identifier, ManifestBuilder, client::error::ClientError, media_type::MEDIA_TYPE_OCI_EMPTY_CONFIG,
    media_type::MEDIA_TYPE_OCI_IMAGE_MANIFEST, tag::InternalTag,
};

use super::super::{PackageManager, error::PackageErrorKind};

// ── The `__ocx.patch` wire shape ──────────────────────────────────────────────

/// Pushes a `__ocx.patch` descriptor artifact to the patch registry.
///
/// Builds an OCI ImageManifest with `artifactType` set to
/// [`crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE`], an empty `{}` config blob,
/// and a single layer carrying the descriptor JSON
/// ([`crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE`]). The artifact is
/// pushed to the `__ocx.patch` internal tag on `patch_repo_id`.
///
/// `descriptor_bytes` is validated by parsing it as a [`PatchDescriptor`]
/// before any network call — a malformed descriptor is rejected up front rather
/// than published.
///
/// A free function over [`ocx_oci::Client`], not a method on it: the `__ocx.patch`
/// artifact's shape is the patch tier's vocabulary, and the client supplies
/// only blob and manifest primitives.
///
/// Returns the manifest digest of the pushed `__ocx.patch` artifact.
///
/// # Errors
///
/// - [`ClientError::InvalidManifest`] — `descriptor_bytes` is not a valid
///   patch descriptor, or manifest assembly failed.
/// - [`ClientError::Authentication`] / [`ClientError::Registry`] — auth or a
///   blob/manifest push failed.
async fn push_patch_descriptor(
    client: &ocx_oci::Client,
    patch_repo_id: &Identifier,
    descriptor_bytes: &[u8],
) -> crate::Result<ocx_oci::Digest> {
    // Validate the descriptor parses before pushing — reject malformed input.
    PatchDescriptor::from_json_bytes(descriptor_bytes)
        .map_err(|e| ClientError::InvalidManifest(format!("invalid patch descriptor: {e}")))?;

    // The patch registry is the one `[patches]` names, written as named: a
    // descriptor is not a package, and no index serves one.
    let patch_identifier = ocx_oci::OciIdentifier::passthrough(patch_repo_id).clone_with_tag(InternalTag::PATCH_TAG);
    // Push stays canonical (mirror-free): remote/proxy mirrors are read-only —
    // `ensure_auth` routes a `Push` scope to the canonical host for that reason.
    client
        .ensure_auth(&patch_identifier, ocx_oci::RegistryOperation::Push)
        .await?;

    let config_data = b"{}".to_vec();
    let config_digest = Algorithm::Sha256.hash(&config_data);
    client.push_blob(&patch_identifier, config_data, &config_digest).await?;

    let layer_len = descriptor_bytes.len();
    let layer_digest = Algorithm::Sha256.hash(descriptor_bytes);
    client
        .push_blob(&patch_identifier, descriptor_bytes.to_vec(), &layer_digest)
        .await?;

    let layer_size = i64::try_from(layer_len)
        .map_err(|_| ClientError::InvalidManifest(format!("descriptor blob size {layer_len} exceeds i64::MAX")))?;
    let layers = vec![ocx_oci::Descriptor {
        media_type: crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE.to_string(),
        digest: layer_digest.to_string(),
        size: layer_size,
        urls: None,
        artifact_type: None,
        annotations: Some(
            [(
                ocx_oci::annotations::TITLE.to_string(),
                InternalTag::PATCH_TAG.to_string(),
            )]
            .into(),
        ),
    }];

    let parts = ManifestBuilder::new()
        .artifact_type(crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE)
        .config_bytes(MEDIA_TYPE_OCI_EMPTY_CONFIG, b"{}".to_vec())
        .layers(layers)
        .build()?;
    // Sanity: the empty-config blob digest computed by the builder must
    // match the one we already pushed above.
    debug_assert_eq!(parts.config_digest.to_string(), config_digest.to_string());
    let manifest_digest = parts.manifest_digest.clone();

    // Push to the tag reference directly (not by digest) so the tag is created.
    client
        .push_manifest_raw(&patch_identifier, parts.manifest_bytes, MEDIA_TYPE_OCI_IMAGE_MANIFEST)
        .await?;

    log::debug!(
        "Pushed patch descriptor for {} (manifest: {})",
        patch_repo_id,
        manifest_digest
    );
    Ok(manifest_digest)
}

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
    pub manifest_digest: ocx_oci::Digest,
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
    /// 3. Push the descriptor manifest via `push_patch_descriptor` (private to
    ///    this module, hence not a link).
    /// 4. Return the published reference, manifest digest, and rule count.
    ///
    /// `patch_repo_id` is the patch-registry repository (global root or
    /// package-specific sub-path) WITHOUT the `__ocx.patch` tag — the push
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
        let manifest_digest = push_patch_descriptor(client, patch_repo_id, descriptor_bytes)
            .await
            .map_err(PackageErrorKind::Internal)?;

        // Step 4: Build the report.
        let patch_reference = patch_repo_id
            .clone_with_tag(ocx_oci::tag::InternalTag::PATCH_TAG)
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

    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_oci::client::test_transport::StubTransportData;
    use ocx_store::file_structure::FileStructure;

    fn make_offline_manager(ocx_home: &Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
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
    // ── push_patch_descriptor (moved here from `oci/client.rs` by WP-14b) ─────

    fn stub_client(data: &StubTransportData) -> ocx_oci::Client {
        ocx_oci::client::test_transport::stub_client(data)
    }

    /// `push_patch_descriptor` pushes a `__ocx.patch` manifest with the
    /// expected artifactType + a descriptor layer, and returns the manifest
    /// digest. Verified against the `StubTransport` via `capture_pushes`.
    #[tokio::test]
    async fn push_patch_descriptor_pushes_patch_artifact_and_returns_digest() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let client = stub_client(&data);

        let descriptor_bytes = valid_descriptor_bytes();

        // Global patch repo identifier (reserved `global` repository at the patch registry).
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let digest = push_patch_descriptor(&client, &patch_repo, &descriptor_bytes)
            .await
            .expect("push_patch_descriptor must succeed");

        // A manifest was pushed.
        let inner = data.read();
        assert!(
            inner.calls.iter().any(|c| c == "push_manifest_raw"),
            "push_patch_descriptor must push a manifest; calls = {:?}",
            inner.calls
        );

        // The descriptor layer blob was pushed (push_blob:<layer_digest>).
        let layer_digest = Algorithm::Sha256.hash(&descriptor_bytes).to_string();
        assert!(
            inner.calls.iter().any(|c| c == &format!("push_blob:{layer_digest}")),
            "push_patch_descriptor must push the descriptor layer blob; calls = {:?}",
            inner.calls
        );

        // The captured manifest carries the patch artifactType + the descriptor layer media type.
        let (_image, (manifest_bytes, manifest_digest)) = inner
            .manifests
            .iter()
            .next()
            .expect("a manifest must have been captured");
        let manifest: ocx_oci::ImageManifest =
            serde_json::from_slice(manifest_bytes).expect("captured manifest must parse");
        assert_eq!(
            manifest.artifact_type.as_deref(),
            Some(crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE),
            "manifest artifactType must be the patch artifact type"
        );
        assert_eq!(manifest.layers.len(), 1, "patch manifest must have exactly one layer");
        assert_eq!(
            manifest.layers[0].media_type,
            crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE,
            "layer media type must be the descriptor layer media type"
        );

        // The returned digest matches the pushed manifest's digest.
        assert_eq!(
            digest.to_string(),
            *manifest_digest,
            "returned digest must equal the pushed manifest digest"
        );
    }

    /// `push_patch_descriptor` rejects malformed descriptor bytes before any push.
    #[tokio::test]
    async fn push_patch_descriptor_rejects_malformed_descriptor() {
        let data = StubTransportData::new();
        let client = stub_client(&data);
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let result = push_patch_descriptor(&client, &patch_repo, b"not valid json {{{").await;
        assert!(
            matches!(result, Err(crate::Error::OciClient(ClientError::InvalidManifest(_)))),
            "malformed descriptor must be rejected with InvalidManifest, got: {result:?}"
        );
        // No manifest was pushed.
        assert!(
            data.read().calls.iter().all(|c| c != "push_manifest_raw"),
            "no manifest must be pushed when the descriptor is malformed"
        );
    }

    /// The descriptor push authenticates with `Push` scope before it touches a
    /// blob or a manifest — the same property the `oci/client.rs` test module
    /// held while this orchestration lived there.
    #[tokio::test]
    async fn push_patch_descriptor_authenticates_with_push_first() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let client = stub_client(&data);
        let patch_repo = Identifier::new_registry("global", "patches.example.com");

        let _ = push_patch_descriptor(&client, &patch_repo, &valid_descriptor_bytes()).await;

        let inner = data.read();
        assert!(
            !inner.auth_calls.is_empty(),
            "push_patch_descriptor must call ensure_auth"
        );
        assert!(
            matches!(inner.auth_calls[0].1, ocx_oci::RegistryOperation::Push),
            "the first ensure_auth must carry Push scope, got {:?}",
            inner.auth_calls[0].1
        );
        // `auth_calls` is its own log, so its first entry is `Push` whether the
        // handshake preceded the blob upload or followed it. `first_call` is the
        // one observation that orders the two against each other.
        assert_eq!(
            inner.first_call.as_deref(),
            Some("ensure_auth"),
            "the handshake must precede every transport call, got {:?} first (calls: {:?})",
            inner.first_call,
            inner.calls
        );
        assert!(
            inner.calls.iter().any(|c| c.starts_with("push_blob:")),
            "a blob push must actually have happened, else the ordering above is vacuous; calls: {:?}",
            inner.calls
        );
    }
}
