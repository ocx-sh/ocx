// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sign pipeline — 15-step push state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md):
//! resolve identifier → acquire OIDC token → generate ephemeral key → Fulcio
//! CSR → signed cert → sign target digest → Rekor upload → bundle → push bundle
//! blob → push referrer manifest → emit [`SignResult`].
//!
//! The pipeline is a thin orchestrator: the cryptographic signing is
//! delegated to a [`Signer`] trait object (injected via
//! [`SignContext::signer`]), and the registry writes go through the
//! [`OciTransport`] also injected via the context.
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use super::error::SignError;
use super::oidc::TokenProvider;
use super::signer::Signer;
use crate::oci::client::OciTransport;
use crate::oci::index::Index;
use crate::oci::{Descriptor, Identifier, Platform};

/// Context passed into [`SignPipeline::run`] — all external dependencies.
///
/// Every field is a `&` borrow so the context does not take ownership of the
/// injected components. The `fulcio_url` / `rekor_url` fields are the
/// C-S1-3 injection seams: production callers use the default Sigstore URLs,
/// tests inject `http://127.0.0.1:<port>` of the fake helpers.
pub struct SignContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
    /// Signer producing the cryptographic bundle.
    pub signer: &'a dyn Signer,
    /// OIDC token provider (used by the signer; surfaced here for diagnostics).
    pub token_provider: &'a dyn TokenProvider,
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Registry transport.
    pub transport: &'a dyn OciTransport,
    /// Index for resolving tag → digest.
    pub index: &'a Index,
    /// Fulcio URL (C-S1-3 injection seam).
    ///
    /// Default: `https://fulcio.sigstore.dev/api/v2/signingCert`.
    pub fulcio_url: &'a str,
    /// Rekor URL (C-S1-3 injection seam).
    ///
    /// Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a str,
}

/// Result emitted by a successful sign pipeline run.
pub struct SignResult {
    /// Digest of the target manifest the signature was attached to.
    pub subject_digest: crate::oci::Digest,
    /// Digest of the pushed Sigstore bundle blob.
    pub bundle_digest: crate::oci::Digest,
    /// Typed digest of the pushed referrer manifest.
    ///
    /// This is the canonical typed field consumed by the CLI layer (e.g.,
    /// `SignatureReport::new`). The full [`Descriptor`] is retained in
    /// `referrer_descriptor` for cases that need size / media-type metadata.
    pub referrer_digest: crate::oci::Digest,
    /// Full OCI descriptor of the pushed referrer manifest (includes digest,
    /// size, and media-type). Kept for callers that need the complete descriptor;
    /// prefer `referrer_digest` when only the digest is required.
    pub referrer_descriptor: Descriptor,
    /// Cert SAN (identity) that signed the target.
    pub certificate_identity: String,
    /// Cert issuer (`--certificate-oidc-issuer` comparand).
    pub certificate_oidc_issuer: String,
}

/// Sign pipeline entry point.
pub struct SignPipeline;

impl SignPipeline {
    /// Run the 15-step sign pipeline.
    pub async fn run(_ctx: SignContext<'_>) -> Result<SignResult, SignError> {
        unimplemented!("SignPipeline::run — Phase 5 implements the 15-step push state machine")
    }
}
