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

use crate::oci::Descriptor;
use crate::oci::sign::error::SignErrorKind;

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
    /// `payload` is the descriptor of the pushed payload blob.
    pub fn build(_subject: Descriptor, _artifact_type: &str, _payload: Descriptor) -> Self {
        unimplemented!("ReferrerManifest::build — Phase 5 implementation")
    }

    /// Serialize the manifest to canonical JSON bytes for push.
    ///
    /// # Errors
    ///
    /// Returns [`SignErrorKind::Internal`] when JSON serialization fails.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, SignErrorKind> {
        unimplemented!("ReferrerManifest::to_canonical_json — Phase 5 implementation")
    }
}
