// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline — full keyless Sigstore verification state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md):
//! list referrers → pick v0.3 bundle → parse → verify cert chain vs TUF root
//! → verify Rekor SET → verify signature over subject digest → identity/issuer
//! match → emit [`VerifyResult`].
//!
//! The pipeline's `rekor_url` + `trust_root` injection seams (C-S1-3) let
//! unit tests and acceptance tests share the same test doubles.
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use super::error::VerifyError;
use super::trust_root::TrustRoot;
use crate::oci::client::OciTransport;
use crate::oci::index::Index;
use crate::oci::{Identifier, Platform};

/// Context passed into [`VerifyPipeline::run`] — all external dependencies.
///
/// `trust_root` is a `&TrustRoot` per C-S1-3: the CLI layer resolves the
/// trust root (production → [`TrustRoot::load_embedded`]; tests →
/// [`TrustRoot::load_from_pem`] against the fake_fulcio root) and passes a
/// borrowed reference so unit and acceptance tests share the same seam.
///
/// `rekor_url` mirrors [`super::super::sign::pipeline::SignContext::rekor_url`]
/// so sign-then-verify tests can target the same fake helper.
pub struct VerifyContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
    /// Expected certificate SAN (user flag).
    pub certificate_identity: &'a str,
    /// Expected certificate OIDC issuer (user flag).
    pub certificate_oidc_issuer: &'a str,
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Registry transport.
    pub transport: &'a dyn OciTransport,
    /// Index for resolving tag → digest.
    pub index: &'a Index,
    /// Trust root (resolved by the CLI layer; C-S1-3 injection seam).
    pub trust_root: &'a TrustRoot,
    /// Rekor URL (C-S1-3 injection seam). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a str,
}

/// Result emitted by a successful verify pipeline run.
pub struct VerifyResult {
    /// Digest of the subject manifest that was verified.
    pub subject_digest: crate::oci::Digest,
    /// Digest of the referrer manifest (the bundle referrer).
    pub referrer_digest: crate::oci::Digest,
    /// Cert SAN that signed the subject.
    pub certificate_identity: String,
    /// Cert OIDC issuer URL.
    pub certificate_oidc_issuer: String,
    /// Rekor integrated time (UTC epoch seconds) of the signature entry.
    pub signed_at: u64,
    /// True when the signing cert had expired by verify time but Rekor SET
    /// attests it was integrated pre-expiry (so verification still succeeds).
    pub cert_expired_but_tlog_valid: bool,
}

/// Verify pipeline entry point.
pub struct VerifyPipeline;

impl VerifyPipeline {
    /// Run the verify pipeline against a [`VerifyContext`].
    pub async fn run(_ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyError> {
        unimplemented!("VerifyPipeline::run — Phase 5 implements the full verify state machine")
    }
}
