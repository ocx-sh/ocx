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
//! Phase 5a wires the non-network plumbing: flag parsing, identifier
//! resolution, and a scoped `VerifyErrorKind::TrustRootUnavailable` /
//! `BundleNotFound` surface. The full verify state machine
//! (`VerifyPipeline::run`) is Phase 5c, blocked on sigstore-rs integration
//! and a public `Client::transport()` accessor.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;
use ocx_lib::oci::sign::endpoint::validate_sigstore_url;
use ocx_lib::oci::verify::{VerifyError, VerifyErrorKind};

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
        let _rekor_url = validate_sigstore_url(&self.rekor_url, "--rekor-url").map_err(|reason| {
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
        let (_index, _client) = context.online_context()?;

        // Phase 5c blocker: `VerifyPipeline::run` requires a `&dyn OciTransport`
        // (see `oci::verify::VerifyContext::transport`) plus a populated
        // `TrustRoot` (`TrustRoot::load_embedded` is also Phase 5c, blocked on
        // the `sigstore-trust-root` crate). Until both land we surface
        // `VerifyErrorKind::TrustRootUnavailable` so the exit-code classifier
        // produces `ConfigError` (78) — a readable "the verify path isn't
        // wired yet" signal rather than a panic. (Verify reuses the existing
        // `TrustRootUnavailable` variant because the trust root is genuinely
        // missing in Slice 1 — there is no value in introducing a separate
        // verify-side `NotImplemented` until the embedded TUF root ships.)
        Err(anyhow::Error::from(VerifyError::new(
            identifier,
            VerifyErrorKind::TrustRootUnavailable,
        )))
    }
}
