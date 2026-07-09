// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package verify` — keyless Sigstore verification of a target manifest's
//! signature via OCI Referrers.
//!
//! Fetches the Sigstore bundle v0.3 referrer for the target, verifies the
//! Fulcio cert chain against the embedded TUF trust root, verifies the Rekor
//! SET, verifies the signature over the subject digest, and checks the cert
//! identity + issuer against user-supplied flags. See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full state machine.
//!
//! There are **no default** `--certificate-identity` / `--certificate-oidc-issuer`
//! values — keyless verification is meaningless without knowing whose
//! signature you trust.
//!
//! This command resolves the identifier, validates `--rekor-url` (SSRF guard),
//! resolves the trust root (`--trust-root` flag, then the
//! `OCX_SIGSTORE_TRUST_ROOT` env var, then the stubbed embedded root), and
//! drives [`VerifyPipeline::run`], which runs the full state machine and
//! returns a [`VerificationReport`]. The positive path is currently exercised
//! only against the fake Sigstore stack; production hardening against
//! public-good Fulcio/Rekor/TUF is tracked separately.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;
use ocx_lib::oci::endpoint::validate_sigstore_url;
use ocx_lib::oci::verify::{TrustRoot, VerifyContext, VerifyError, VerifyErrorKind, VerifyPipeline};

use crate::api::data::verification::VerificationReport;
use crate::options;

/// Default public Rekor transparency-log endpoint (overridable via `--rekor-url`).
const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";

#[derive(Parser, Clone)]
pub struct Verify {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    /// Expected cert SAN (required). Exact-match only in Slice 1.
    ///
    /// Example: `you@example.com`, `https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main`.
    #[clap(long = "certificate-identity", value_name = "IDENTITY", required = true)]
    certificate_identity: String,

    /// Expected cert OIDC issuer (required). Exact-match only in Slice 1.
    ///
    /// Example: `https://github.com/login/oauth`, `https://token.actions.githubusercontent.com`.
    #[clap(long = "certificate-oidc-issuer", value_name = "URL", required = true)]
    certificate_oidc_issuer: String,

    /// Rekor transparency-log endpoint (C-S1-3 injection seam, defaults to public Rekor).
    #[clap(long = "rekor-url", value_name = "URL", default_value = DEFAULT_REKOR_URL)]
    rekor_url: String,

    /// Bypass the referrers-capability cache for this invocation.
    #[clap(long = "no-cache")]
    no_cache: bool,

    /// Trust-root override: a PEM file of Fulcio CA certificate(s).
    ///
    /// By default verification uses the bundled Sigstore trust root. This flag
    /// (or the `OCX_SIGSTORE_TRUST_ROOT` env var) points at a custom Fulcio CA
    /// PEM for air-gapped deployments or against a private Sigstore instance.
    /// The flag takes precedence over the env var.
    #[clap(long = "trust-root", value_name = "PATH")]
    trust_root: Option<std::path::PathBuf>,

    /// Package identifier to verify (`registry/repo:tag[@digest]`).
    identifier: options::Identifier,
}

impl Verify {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // SSRF hardening (CWE-918): validate user-supplied endpoint URL at the
        // boundary before it becomes an HTTP client target. Wrap the
        // UrlRejection into `VerifyErrorKind::InvalidEndpointUrl` so the
        // exit-code classifier maps it to `UsageError` (64) via the verify
        // error path — no cross-subsystem dependency on SignError.
        let rekor_url = validate_sigstore_url(&self.rekor_url, "--rekor-url").map_err(|reason| {
            VerifyError::new(
                identifier.clone(),
                VerifyErrorKind::InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason,
                },
            )
        })?;

        // Online-only: verify needs the registry to fetch referrers (and Rekor
        // to verify the SET). Offline mode → exit 81 via `OfflineMode` classifier.
        let (index, client) = context.online_context()?;

        let trust_root = self.resolve_trust_root(&identifier)?;

        let verify_context = VerifyContext {
            identifier: &identifier,
            platform: &self.platform,
            certificate_identity: &self.certificate_identity,
            certificate_oidc_issuer: &self.certificate_oidc_issuer,
            no_cache: self.no_cache,
            transport: client.transport(),
            index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            cache_root: context.file_structure().root(),
        };
        let result = VerifyPipeline::run(verify_context).await?;

        let report = VerificationReport::new(
            result.subject_digest,
            result.referrer_digest,
            result.certificate_identity,
            result.certificate_oidc_issuer,
            iso8601(result.signed_at),
        );
        context.api().report(&report)?;
        Ok(ExitCode::SUCCESS)
    }

    /// Resolve the trust root: `--trust-root` flag > `OCX_SIGSTORE_TRUST_ROOT`
    /// env > the bundled Sigstore root. The override is a PEM of Fulcio CA
    /// certificate(s).
    fn resolve_trust_root(&self, identifier: &oci::Identifier) -> anyhow::Result<TrustRoot> {
        let override_path = self
            .trust_root
            .clone()
            .or_else(|| std::env::var_os("OCX_SIGSTORE_TRUST_ROOT").map(std::path::PathBuf::from));
        match override_path {
            Some(path) => {
                let bytes = std::fs::read(&path).map_err(|source| {
                    VerifyError::new(
                        identifier.clone(),
                        VerifyErrorKind::TrustRootLoad(
                            ocx_lib::oci::verify::error::TrustRootLoadReason::AssetReadFailed {
                                source: Box::new(source),
                            },
                        ),
                    )
                })?;
                TrustRoot::load_from_pem(&bytes)
                    .map_err(|kind| anyhow::Error::from(VerifyError::new(identifier.clone(), kind)))
            }
            None => TrustRoot::load_embedded()
                .map_err(|kind| anyhow::Error::from(VerifyError::new(identifier.clone(), kind))),
        }
    }
}

/// Format a UTC epoch-seconds timestamp as ISO-8601 (`YYYY-MM-DDThh:mm:ssZ`).
fn iso8601(epoch_secs: u64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs as i64, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}
