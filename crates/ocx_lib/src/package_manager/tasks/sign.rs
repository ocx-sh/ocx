// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `sign_one` — package-manager task that signs a single target manifest.
//!
//! Wraps [`crate::oci::sign::SignPipeline`] (C-S1-3 pipeline with injection
//! seams) in the three-layer error model: transport / index come from the
//! [`PackageManager`] facade, the pipeline's [`SignResult`] becomes a
//! [`SignReport`], and any failure is wrapped in a
//! [`PackageError`] tagged with the target identifier.
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — tasks live in `package_manager/tasks/`; the aggregator is
//! `package_manager/tasks.rs` (not `tasks/mod.rs`).
//!
//! Phase 5c stub — body uses `unimplemented!()`.

use crate::oci::{self, sign::pipeline::SignResult};
use crate::package_manager::error::{PackageError, PackageErrorKind};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::sign_one`].
///
/// `fulcio_url` / `rekor_url` are the Sigstore endpoints (C-S1-3 injection
/// seams — default to the public Fulcio/Rekor URLs, overridden by tests).
///
/// `identity_token` is the precedence-resolved override token from the CLI
/// layer (`--identity-token-file` > `--identity-token-stdin` > env). When
/// `None`, the dispatching token provider falls back to ambient detection
/// (GHA, GitLab, CircleCI, …) then optionally to a browser OAuth flow when
/// `no_tty` is `false`. See C-S1-4.
pub struct SignOptions {
    /// Fulcio CA endpoint. Default: `https://fulcio.sigstore.dev`.
    pub fulcio_url: String,
    /// Rekor transparency log endpoint. Default: `https://rekor.sigstore.dev`.
    pub rekor_url: String,
    /// OIDC override token (file / stdin / env, resolved by CLI layer).
    pub identity_token: Option<String>,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// When true, suppress the browser OAuth fallback (CI / headless).
    pub no_tty: bool,
}

/// Success payload returned by [`PackageManager::sign_one`].
///
/// Thin wrapper over [`SignResult`] so the package-manager layer owns the
/// report type and the CLI `Printable` impl lives in `ocx_cli::api::data`.
pub struct SignReport {
    /// Raw pipeline result (subject digest, referrer descriptor, cert identity).
    pub result: SignResult,
}

impl PackageManager {
    /// Sign `package` for `platform` by publishing a Sigstore bundle v0.3
    /// referrer manifest to the registry.
    ///
    /// The pipeline is:
    /// resolve subject digest → pre-check OIDC → Fulcio keyless cert issue
    /// → sign subject digest → Rekor upload → bundle build → push bundle
    /// blob → push referrer manifest via the OCI Referrers API only. Registries
    /// without Referrers API hard-fail with `SignErrorKind::ReferrersUnsupported`
    /// (exit 84) per ADR S1-F — OCX never writes `sha256-<hex>.sig` fallback
    /// tags on the push side. Emits a [`SignReport`] on success.
    ///
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::sign::SignErrorKind`].
    ///
    /// Phase 5c stub — Phase 5c implements the pipeline.
    pub async fn sign_one(
        &self,
        _package: &oci::Identifier,
        _platform: &oci::Platform,
        _opts: SignOptions,
    ) -> Result<SignReport, PackageError> {
        unimplemented!(
            "PackageManager::sign_one — Phase 5 wires SignPipeline::run with transport/index from \
             the facade and wraps errors in PackageError"
        )
    }
}

#[allow(dead_code)]
fn _map_sign_error(identifier: oci::Identifier, err: crate::oci::sign::SignError) -> PackageError {
    // Phase 5: concrete mapping. Today the wrapper exists so the return-type
    // of `sign_one` is stable and call sites compile.
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Sign(Box::new(err))),
    )
}
