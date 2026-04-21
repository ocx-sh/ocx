// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! [`Signer`] trait — OIDC acquisition separated from bundle push.
//!
//! Per R1 fix pass (Architect F2): the signer produces a signed bundle for a
//! target digest. The referrer push is a separate concern owned by
//! [`pipeline::SignPipeline`](super::pipeline). This split lets tests inject a
//! fake signer without standing up fake Fulcio/Rekor servers.
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use async_trait::async_trait;

use super::bundle::SignedBundle;
use super::error::SignErrorKind;
use crate::oci::Digest;

/// Signer trait — produces a Sigstore bundle for a target digest.
///
/// Separates the "how to get a cryptographic signature" concern from the "how
/// to push the resulting bundle to the registry" concern. Multiple signers
/// can compose the same push pipeline (keyless, HSM/KMS in v2).
#[async_trait]
pub trait Signer: Send + Sync {
    /// Sign the given target digest, returning a Sigstore bundle v0.3.
    async fn sign(&self, target_digest: &Digest) -> Result<SignedBundle, SignErrorKind>;

    /// Static string identifying this signer kind (e.g., `keyless-fulcio`).
    ///
    /// Surfaced in the JSON error envelope's `context.signer` field so
    /// operators can distinguish signer classes.
    fn signer_kind(&self) -> &'static str;
}

/// Keyless signer composing a [`TokenProvider`] + Fulcio + Rekor.
///
/// The reference implementation of [`Signer`] for Slice 1. Phase 1 stub.
/// Phase 5c will re-add the token provider and URL fields alongside the real
/// Fulcio/Rekor call sites.
pub struct KeylessSigner;

impl KeylessSigner {
    /// Construct a keyless signer.
    ///
    /// Phase 5c will re-introduce `token_provider`, `fulcio_url`, and
    /// `rekor_url` parameters alongside the real signing implementation.
    pub fn new() -> Self {
        Self
    }
}

impl Default for KeylessSigner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Signer for KeylessSigner {
    async fn sign(&self, _target_digest: &Digest) -> Result<SignedBundle, SignErrorKind> {
        unimplemented!("KeylessSigner::sign — Phase 5 implements keyless signing pipeline")
    }

    fn signer_kind(&self) -> &'static str {
        "keyless-fulcio"
    }
}
