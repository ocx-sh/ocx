// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx verify` — keyless Sigstore verification of a target manifest's
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
//! Phase 1 stub — body uses `unimplemented!()`.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;

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
    pub async fn execute(&self, _context: crate::app::Context) -> anyhow::Result<ExitCode> {
        unimplemented!(
            "Verify::execute — Phase 5 calls PackageManager::verify_one and \
             reports via api.report_verification"
        )
    }
}
