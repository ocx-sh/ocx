// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `sign_one` — package-manager task that signs a single target manifest.
//!
//! Wraps [`crate::oci::sign::SignPipeline`] (C-S1-3 pipeline with injection
//! seams) in the three-layer error model: the client / index come from the
//! [`PackageManager`] facade, the pipeline's [`SignResult`] becomes a
//! [`SignReport`], and any failure is wrapped in a [`PackageError`] tagged with
//! the target identifier.
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — tasks live in `package_manager/tasks/`; the aggregator is
//! `package_manager/tasks.rs` (not `tasks/mod.rs`).

use url::Url;
use zeroize::Zeroizing;

use crate::oci::sign::{DispatchingTokenProvider, KeylessSigner, SignContext, SignError, SignPipeline};
use crate::oci::{self, sign::pipeline::SignResult};
use crate::package_manager::error::{PackageError, PackageErrorKind};

use super::super::PackageManager;

/// Options forwarded from the CLI to [`PackageManager::sign_one`].
///
/// `fulcio_url` / `rekor_url` are the validated Sigstore endpoints (C-S1-3
/// injection seams — default to the public Fulcio/Rekor URLs, overridden by
/// tests). The CLI performs SSRF validation at its boundary and hands over the
/// parsed [`Url`]s.
///
/// `identity_token` is the precedence-resolved override token from the CLI
/// layer (`--identity-token-file` > `--identity-token-stdin` > env), held under
/// [`Zeroizing`] so the cleartext is scrubbed on drop. When `None`, the
/// dispatching token provider falls back to ambient detection (GHA, GitLab,
/// CircleCI, …) then optionally to a browser OAuth flow when `no_tty` is
/// `false`. See C-S1-4.
pub struct SignOptions {
    /// Fulcio CA endpoint (validated by the CLI). Default: `https://fulcio.sigstore.dev`.
    pub fulcio_url: Url,
    /// Rekor transparency log endpoint (validated by the CLI). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: Url,
    /// OIDC override token (file / stdin / env, resolved by the CLI layer).
    pub identity_token: Option<Zeroizing<String>>,
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
    /// The registry client comes from the facade ([`require_client`][Self::require_client]);
    /// signing requires network access, so an offline manager fails with
    /// `OfflineMode` (exit 81). (`ocx package sign` refuses `--offline` earlier
    /// with a dedicated policy error.)
    ///
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::sign::SignErrorKind`].
    pub async fn sign_one(
        &self,
        package: &oci::Identifier,
        platform: &oci::Platform,
        opts: SignOptions,
    ) -> Result<SignReport, PackageError> {
        let client = self
            .require_client()
            .map_err(|e| PackageError::new(package.clone(), PackageErrorKind::Internal(e)))?;

        let signer = KeylessSigner::new();
        let token_provider = DispatchingTokenProvider::new(opts.identity_token, opts.no_tty);
        let context = SignContext {
            identifier: package,
            platform,
            signer: &signer,
            token_provider: &token_provider,
            no_cache: opts.no_cache,
            index: self.index(),
            fulcio_url: &opts.fulcio_url,
            rekor_url: &opts.rekor_url,
            state: &self.file_structure().state,
        };
        let result = SignPipeline::run(client, context)
            .await
            .map_err(|err| map_sign_error(package.clone(), err))?;
        Ok(SignReport { result })
    }
}

/// Wrap a [`SignError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the sign exit code through `PackageErrorKind::Internal`.
fn map_sign_error(identifier: oci::Identifier, err: SignError) -> PackageError {
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Sign(Box::new(err))),
    )
}
