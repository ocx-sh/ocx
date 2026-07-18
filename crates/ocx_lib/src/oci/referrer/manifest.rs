// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI referrer manifest (image manifest carrying a `subject` descriptor).
//!
//! Phase 1 stub — shape only. The `ReferrerManifest` represents an OCI 1.1
//! image manifest whose `subject` field points at the target being referred
//! to (signature, SBOM, attestation). See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full push-side state machine.

use serde::{Deserialize, Serialize};

use super::media_types::{EMPTY_CONFIG, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_SIZE};
use crate::oci::sign::error::SignErrorKind;
use crate::oci::{Descriptor, OCI_IMAGE_MEDIA_TYPE};

/// OCI 1.1 image manifest carrying a `subject` descriptor.
///
/// Serializes to an OCI image manifest (`application/vnd.oci.image.manifest.v1+json`)
/// with `artifactType` set to the referrer's media type (e.g.
/// [`SIGSTORE_BUNDLE_V03`](super::media_types::SIGSTORE_BUNDLE_V03)) and a
/// `subject` descriptor identifying the manifest being referred to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferrerManifest {
    /// OCI schema version (always `2` for OCI 1.x manifests).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u8,

    /// Top-level media type (`application/vnd.oci.image.manifest.v1+json`).
    #[serde(rename = "mediaType")]
    pub media_type: String,

    /// Artifact-specific type (e.g., [`SIGSTORE_BUNDLE_V03`](super::media_types::SIGSTORE_BUNDLE_V03)).
    #[serde(rename = "artifactType")]
    pub artifact_type: String,

    /// Empty-config descriptor per OCI empty-descriptor convention.
    pub config: Descriptor,

    /// Referrer payload layers (e.g., the Sigstore bundle blob).
    pub layers: Vec<Descriptor>,

    /// Descriptor of the subject this referrer refers to.
    pub subject: Descriptor,
}

impl ReferrerManifest {
    /// Build a referrer manifest for the given subject with a single payload layer.
    ///
    /// `artifact_type` is the referrer's media type (e.g.
    /// [`SIGSTORE_BUNDLE_V03`](super::media_types::SIGSTORE_BUNDLE_V03)).
    /// `payload` is the descriptor of the pushed payload blob. The config is the
    /// OCI empty-config descriptor per the empty-descriptor convention.
    pub fn build(subject: Descriptor, artifact_type: &str, payload: Descriptor) -> Self {
        let config = Descriptor {
            media_type: EMPTY_CONFIG.to_string(),
            digest: EMPTY_CONFIG_DIGEST.to_string(),
            size: EMPTY_CONFIG_SIZE as i64,
            ..Descriptor::default()
        };
        Self {
            schema_version: 2,
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            artifact_type: artifact_type.to_string(),
            config,
            layers: vec![payload],
            subject,
        }
    }

    /// Serialize the manifest to JSON bytes for push.
    ///
    /// The registry addresses the referrer by the SHA-256 of exactly these
    /// bytes, so the caller must digest the same buffer it pushes.
    ///
    /// # Errors
    ///
    /// Returns [`SignErrorKind::Internal`] when JSON serialization fails.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, SignErrorKind> {
        serde_json::to_vec(self).map_err(|e| SignErrorKind::Internal(Box::new(e)))
    }
}
