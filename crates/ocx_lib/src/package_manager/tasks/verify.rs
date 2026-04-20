// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `verify_one` — package-manager task that verifies a target manifest's
//! Sigstore signature against a trust root.
//!
//! Wraps [`crate::oci::verify::VerifyPipeline`] (C-S1-3 pipeline with
//! injection seams). On success emits a [`VerifyReport`]; on failure wraps
//! the [`VerifyError`] in a [`PackageError`] so the caller sees per-package
//! diagnosis with an exit code via [`crate::oci::verify::VerifyErrorKind`].
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — the aggregator is `package_manager/tasks.rs` (not
//! `tasks/mod.rs`).
//!
//! Phase 1 stub — body uses `unimplemented!()`.

use crate::oci::{self, verify::VerifyResult};
use crate::package_manager::error::{PackageError, PackageErrorKind};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::verify_one`].
///
/// `certificate_identity` and `certificate_oidc_issuer` are required user
/// flags — there is no default identity for keyless verification, every
/// call must explicitly state whose signature it trusts.
///
/// `rekor_url` is the C-S1-3 Rekor injection seam (defaults to the public
/// Rekor endpoint; fake Rekor in tests).
pub struct VerifyOptions {
    /// Expected cert SAN (user flag `--certificate-identity`).
    pub certificate_identity: String,
    /// Expected cert OIDC issuer (user flag `--certificate-oidc-issuer`).
    pub certificate_oidc_issuer: String,
    /// Rekor transparency log endpoint (C-S1-3 injection seam).
    pub rekor_url: String,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
}

/// Success payload returned by [`PackageManager::verify_one`].
///
/// Thin wrapper over [`VerifyResult`] so the package-manager layer owns the
/// report type and the CLI `Printable` impl lives in `ocx_cli::api::data`.
pub struct VerifyReport {
    /// Raw pipeline result (subject digest, referrer digest, cert identity,
    /// signed-at timestamp, `cert_expired_but_tlog_valid` flag).
    pub result: VerifyResult,
}

impl PackageManager {
    /// Verify `package` for `platform` against the embedded (or injected)
    /// Sigstore trust root.
    ///
    /// The pipeline is: list referrers → pick v0.3 bundle → parse → verify
    /// cert chain vs TUF root → verify Rekor SET → verify signature over
    /// subject digest → identity/issuer match → emit [`VerifyReport`].
    ///
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::verify::VerifyErrorKind`].
    ///
    /// Phase 1 stub — Phase 5 implements the pipeline.
    pub async fn verify_one(
        &self,
        _package: &oci::Identifier,
        _platform: &oci::Platform,
        _opts: VerifyOptions,
    ) -> Result<VerifyReport, PackageError> {
        unimplemented!(
            "PackageManager::verify_one — Phase 5 wires VerifyPipeline::run with transport/index \
             from the facade and wraps errors in PackageError"
        )
    }
}

#[allow(dead_code)]
fn _map_verify_error(identifier: oci::Identifier, err: crate::oci::verify::VerifyError) -> PackageError {
    // Phase 5: concrete mapping. Today the wrapper exists so the return-type
    // of `verify_one` is stable and call sites compile.
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Verify(Box::new(err))),
    )
}
