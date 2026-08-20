// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package sign` — keyless Sigstore signing of a published package
//! manifest via OCI Referrers.
//!
//! Publishes a Sigstore bundle v0.3 as a referrer manifest for the target,
//! with the bundle body itself in a CAS blob. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full pipeline.
//!
//! C-S1-4 override token handling: the CLI resolves `--identity-token-file` >
//! `--identity-token-stdin` > `OCX_IDENTITY_TOKEN` env *before* calling the
//! sign pipeline. There is deliberately NO `--identity-token <VALUE>` flag —
//! raw tokens on the command line would leak into shell history.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::Error as LibError;
use ocx_lib::oci;
use ocx_lib::oci::sign::SignErrorKind;
use ocx_lib::package_manager::SignOptions;
use ocx_lib::package_manager::error::{PackageError, PackageErrorKind};

use crate::api::data::signature::SignatureReport;
use crate::command::package_sign_common;
use crate::options;

#[derive(Parser, Clone)]
pub struct PackageSign {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    // C-S1-3 injection seam: these two URL overrides point sign at a private
    // Fulcio/Rekor deployment (validated at the boundary in `execute`). Left
    // `Option` rather than clap-defaulted so `execute` can tell "user passed
    // the public default" from "user passed nothing" — the latter is what
    // `[trust.sigstore]` gets to answer.
    /// Fulcio CA endpoint (the keyless certificate issuer)
    ///
    /// Defaults to [trust.sigstore].fulcio_url, else public Fulcio.
    #[clap(long = "fulcio-url", value_name = "URL")]
    fulcio_url: Option<String>,

    /// Rekor transparency-log endpoint
    ///
    /// Defaults to [trust.sigstore].rekor_url, else public Rekor.
    #[clap(long = "rekor-url", value_name = "URL")]
    rekor_url: Option<String>,

    /// Read the OIDC identity token from this file (highest precedence).
    ///
    /// Use this when the CI system writes the token to a file instead of the
    /// environment (GitHub Actions `$ACTIONS_ID_TOKEN_REQUEST_TOKEN` flow is
    /// env-based; other systems write the token out).
    ///
    /// Security: `--identity-token-file` does not follow symlinks. On Unix the
    /// file is opened with `O_NOFOLLOW` and rejected if not owned by the
    /// effective user or if group/other permission bits are set.
    #[clap(
        long = "identity-token-file",
        value_name = "PATH",
        conflicts_with = "identity_token_stdin"
    )]
    identity_token_file: Option<std::path::PathBuf>,

    /// Read the OIDC identity token from stdin (second precedence).
    ///
    /// Mutually exclusive with `--identity-token-file`. Accepts a newline-terminated
    /// token on stdin; trailing whitespace is trimmed.
    #[clap(long = "identity-token-stdin", conflicts_with = "identity_token_file")]
    identity_token_stdin: bool,

    /// Suppress the interactive browser OAuth fallback (CI / headless).
    ///
    /// When set, ambient detection must succeed or the override flags must
    /// supply a token; there is no interactive recovery path.
    #[clap(long = "no-tty")]
    no_tty: bool,

    /// Bypass the referrers-capability cache for this invocation.
    ///
    /// Default: the per-registry capability probe is cached in
    /// `$OCX_HOME/state/referrers/<registry>.json` to avoid repeated 404
    /// probes. `--no-cache` forces a fresh probe, useful after a registry
    /// upgrades to OCI 1.1.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Package identifier to sign (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl PackageSign {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918): validate user-supplied endpoint URLs at the
        // boundary before they become HTTP client targets. Precedence, guard
        // and refusal kind are the shared ladder's — see `resolve_sigstore_pair`.
        let (fulcio_url, rekor_url) = package_sign_common::resolve_sigstore_pair(
            context.config_trust_sigstore(),
            &identifier,
            self.fulcio_url.as_deref(),
            self.rekor_url.as_deref(),
        )?;
        // S1-E policy: offline sign is a deliberate rejection, NOT a passive
        // network-access failure — the acceptance test `test_sign_offline_refused`
        // drives this contract.
        package_sign_common::refuse_when_offline(&context, &identifier, SignErrorKind::OfflineSignRefused)?;

        // C-S1-4 token precedence: file > stdin > env. The resolved token is
        // held under `Zeroizing`; never log, never surface in error context.
        let override_token = package_sign_common::resolve_override_token(
            self.identity_token_file.as_deref(),
            self.identity_token_stdin,
            &identifier,
        )
        .await?;

        // Route through the PackageManager facade: it owns the pipeline assembly
        // (registry client, index, signer, token provider) and returns a
        // per-package error whose kind preserves the sign exit-code taxonomy.
        // Offline was already refused above via `OfflineSignRefused`.
        let options = SignOptions {
            fulcio_url,
            rekor_url,
            identity_token: override_token,
            no_cache: self.no_cache,
            no_tty: self.no_tty,
        };
        let result = context
            .manager()
            .sign_one(&identifier, &self.platform, options)
            .await
            .map_err(sign_error_into_anyhow)?
            .result;

        let report = SignatureReport::new(
            identifier.to_string(),
            result.subject_digest,
            result.bundle_digest,
            result.referrer_digest,
            &self.platform,
            result.certificate_identity,
            result.certificate_oidc_issuer,
        );
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// Convert a sign-path [`PackageError`] into an `anyhow::Error`, unwrapping the
/// inner [`SignError`] so the `--format json` error envelope's
/// `context.identifier` is populated on every pipeline-stage failure — matching
/// the pre-check paths (offline refusal, URL validation) that already surface a
/// bare `SignError`.
///
/// `ocx_lib::Error::Sign` is `#[error(transparent)]`, so its `source()` forwards
/// straight to the inner `SignErrorKind`, skipping the `SignError` node the
/// envelope's context walk downcasts to. The exit code, `error.kind`, and
/// `error.detail` are unchanged — all three reach the same `SignErrorKind`
/// whether or not the `SignError` node is preserved.
fn sign_error_into_anyhow(err: PackageError) -> anyhow::Error {
    match err.kind {
        PackageErrorKind::Internal(LibError::Sign(sign_error)) => anyhow::Error::new(*sign_error),
        kind => anyhow::Error::new(kind),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the sign-path error-envelope contract.

    use super::*;
    use ocx_lib::oci::sign::SignError;

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::parse("registry.example/pkg:1.0").expect("static parse")
    }

    /// A pipeline-stage `SignError` wrapped in a `PackageError` (the shape the
    /// sign facade produces) must still surface `context.identifier` in the
    /// `--format json` envelope.
    ///
    /// Regression guard for the `sign_error_into_anyhow` unwrap — mirror of the
    /// verify-side test. `PackageError` omits `#[source]` and `Error::Sign` is
    /// `#[error(transparent)]`, so only the unwrap re-roots the chain on the bare
    /// `SignError` the envelope's context walk downcasts to. If the unwrap
    /// regresses the identifier vanishes and this fails.
    #[test]
    fn sign_error_wrapped_in_package_error_still_populates_envelope_identifier() {
        use crate::error_envelope::render_error_envelope;

        let id = test_identifier();
        let package_error = PackageError::new(
            id.clone(),
            PackageErrorKind::Internal(LibError::Sign(Box::new(SignError::new(
                id,
                SignErrorKind::OidcTokenRejected,
            )))),
        );
        let err = sign_error_into_anyhow(package_error);
        let json = render_error_envelope("package sign", &err).expect("render envelope");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(parsed["exit_code"], 80);
        assert_eq!(parsed["error"]["kind"], "auth_error");
        assert_eq!(
            parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0",
            "identifier must survive the PackageError wrap → sign_error_into_anyhow unwrap",
        );
    }
}
