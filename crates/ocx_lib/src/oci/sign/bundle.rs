// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sigstore bundle v0.3 JSON builder.
//!
//! Produces the canonical `application/vnd.dev.sigstore.bundle.v0.3+json`
//! payload: cert chain + signature bytes + Rekor SET. The bundle is the
//! referrer's payload layer (see [`super::pipeline::SignPipeline`]).
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use serde::{Deserialize, Serialize};

use super::error::SignErrorKind;
use super::fulcio::FulcioCertificate;
use super::rekor::RekorEntry;

/// Serialized Sigstore bundle v0.3 payload.
///
/// Carries the raw JSON bytes plus the digest of those bytes. Bytes are
/// pushed as a blob; the digest is referenced by the referrer manifest's
/// `layers[0]` entry.
#[derive(Debug, Clone)]
pub struct SignedBundle {
    /// Canonical JSON bytes of the bundle v0.3 document.
    pub bytes: Vec<u8>,
    /// SHA-256 digest of `bytes` as a descriptor-ready string.
    pub digest: String,
}

/// Bundle v0.3 builder.
#[derive(Debug, Default)]
pub struct BundleBuilder {
    // Private state filled by `with_*` methods in Phase 5.
}

impl BundleBuilder {
    /// Start a new bundle builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the Fulcio-issued certificate.
    pub fn with_certificate(self, _cert: FulcioCertificate) -> Self {
        unimplemented!("BundleBuilder::with_certificate — Phase 5 implementation")
    }

    /// Attach the raw signature bytes over the target digest.
    pub fn with_signature(self, _signature: Vec<u8>) -> Self {
        unimplemented!("BundleBuilder::with_signature — Phase 5 implementation")
    }

    /// Attach the Rekor log entry with SET.
    pub fn with_rekor_entry(self, _entry: RekorEntry) -> Self {
        unimplemented!("BundleBuilder::with_rekor_entry — Phase 5 implementation")
    }

    /// Attach the target digest being signed.
    pub fn with_target_digest(self, _digest: &str) -> Self {
        unimplemented!("BundleBuilder::with_target_digest — Phase 5 implementation")
    }

    /// Finalize the bundle, producing canonical JSON bytes.
    pub fn build(self) -> Result<SignedBundle, SignErrorKind> {
        unimplemented!("BundleBuilder::build — Phase 5 serializes per Sigstore bundle v0.3 spec")
    }
}

/// Raw Sigstore bundle v0.3 document (for deserialization + round-trip tests).
///
/// The structural fields are intentionally left as JSON values so Phase 1
/// scaffolding compiles without nailing down the internal shape — Phase 5
/// replaces the inner type with the real sigstore-rs bundle struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleV03 {
    /// `mediaType` top-level field.
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// Opaque verification material JSON object.
    #[serde(rename = "verificationMaterial")]
    pub verification_material: serde_json::Value,
    /// Opaque message signature JSON object.
    #[serde(rename = "messageSignature")]
    pub message_signature: serde_json::Value,
}
