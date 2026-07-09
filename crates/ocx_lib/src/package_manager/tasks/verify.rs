// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `verify_one` — the single lib-level keyless-Sigstore verify entry point.
//!
//! Given a resolved OCI identifier, a platform, and the effective ANY-of trust
//! policy set, drive [`crate::oci::verify::VerifyPipeline::run`] (the #194
//! pipeline) with the #196 trust-root/offline material and wrap any
//! [`VerifyError`](crate::oci::verify::VerifyError) in a [`PackageError`] so the
//! caller sees per-package diagnosis and the exit code survives the batch
//! classifier (`PackageErrorKind::Internal` → `classify_error` →
//! `crate::Error::Verify` → `VerifyError::classify`).
//!
//! Consumed by the policy-gated auto-verify hook (see
//! [`super::auto_verify`]). The `ocx package verify` CLI command drives the
//! pipeline directly because it layers flag-vs-policy identity resolution and
//! flag-driven trust-root overrides on top; both ultimately run the same
//! pipeline.
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — the aggregator is `package_manager/tasks.rs`.

use std::path::Path;

use url::Url;

use crate::oci::client::OciTransport;
use crate::oci::verify::pipeline::VerifyResult;
use crate::oci::verify::{TrustRoot, VerifyContext, VerifyError, VerifyPipeline};
use crate::oci::{self};
use crate::package_manager::error::{PackageError, PackageErrorKind};
use crate::trust::CompiledPolicy;

use super::super::PackageManager;

/// External dependencies forwarded to [`PackageManager::verify_one`].
///
/// `transport` is the **registry** transport (present even under `--offline` —
/// verify inherently reads the artifact + its signature referrer from the
/// registry); the manager's own offline-gated client is not used here.
pub struct VerifyOptions<'a> {
    /// Resolved ANY-of policies the signing certificate must satisfy.
    pub policies: &'a [CompiledPolicy],
    /// Registry transport (always available, unlike the manager's offline client).
    pub transport: &'a dyn OciTransport,
    /// Trust root (Fulcio CA + optional pinned Rekor key); #196 seam.
    pub trust_root: &'a TrustRoot,
    /// Rekor transparency-log endpoint (default public Rekor).
    pub rekor_url: &'a Url,
    /// When true, no Sigstore-trust-services network — the Rekor key must come
    /// from pinned/cached trust material.
    pub offline: bool,
    /// `$OCX_HOME` root for the referrers-capability and trust-root caches.
    pub cache_root: &'a Path,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
}

/// Success payload returned by [`PackageManager::verify_one`].
pub struct VerifyReport {
    /// Raw pipeline result (subject digest, referrer digest, cert identity,
    /// signed-at timestamp, `cert_expired_but_tlog_valid` flag).
    pub result: VerifyResult,
}

impl PackageManager {
    /// Verify `package` for `platform` against `opts.trust_root`, requiring the
    /// signing certificate to satisfy one of `opts.policies` (ANY-of).
    ///
    /// The pipeline is: resolve target → list referrers (capability cache) →
    /// pick the v0.3 bundle → verify cert chain vs trust root → bind signature
    /// to subject digest → verify signature → verify Rekor SET → identity/issuer
    /// match → emit [`VerifyReport`].
    ///
    /// # Errors
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::verify::VerifyErrorKind`].
    pub async fn verify_one(
        &self,
        package: &oci::Identifier,
        platform: &oci::Platform,
        opts: VerifyOptions<'_>,
    ) -> Result<VerifyReport, PackageError> {
        let context = VerifyContext {
            identifier: package,
            platform,
            policies: opts.policies,
            no_cache: opts.no_cache,
            transport: opts.transport,
            index: self.index(),
            trust_root: opts.trust_root,
            rekor_url: opts.rekor_url,
            cache_root: opts.cache_root,
            offline: opts.offline,
        };
        let result = VerifyPipeline::run(context)
            .await
            .map_err(|err| map_verify_error(package.clone(), err))?;
        Ok(VerifyReport { result })
    }
}

/// Wrap a [`VerifyError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the verify exit code through `PackageErrorKind::Internal`.
fn map_verify_error(identifier: oci::Identifier, err: VerifyError) -> PackageError {
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Verify(Box::new(err))),
    )
}
