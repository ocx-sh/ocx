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
//!
//! Phase 1 stub — body uses `unimplemented!()`.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;

use crate::options;

/// Default public Fulcio CA endpoint (overridable via `--fulcio-url`).
const DEFAULT_FULCIO_URL: &str = "https://fulcio.sigstore.dev";
/// Default public Rekor transparency-log endpoint (overridable via `--rekor-url`).
const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";

#[derive(Parser, Clone)]
pub struct PackageSign {
    /// Target platform (single-platform manifest under an image index).
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,

    /// Fulcio CA endpoint (C-S1-3 injection seam, defaults to public Fulcio).
    #[clap(long = "fulcio-url", value_name = "URL", default_value = DEFAULT_FULCIO_URL)]
    fulcio_url: String,

    /// Rekor transparency-log endpoint (C-S1-3 injection seam, defaults to public Rekor).
    #[clap(long = "rekor-url", value_name = "URL", default_value = DEFAULT_REKOR_URL)]
    rekor_url: String,

    /// Read the OIDC identity token from this file (C-S1-4, highest precedence).
    ///
    /// Use this when the CI system writes the token to a file instead of the
    /// environment (GitHub Actions `$ACTIONS_ID_TOKEN_REQUEST_TOKEN` flow is
    /// env-based; other systems write the token out).
    #[clap(
        long = "identity-token-file",
        value_name = "PATH",
        conflicts_with = "identity_token_stdin"
    )]
    identity_token_file: Option<std::path::PathBuf>,

    /// Read the OIDC identity token from stdin (C-S1-4, second precedence).
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
    pub async fn execute(&self, _context: crate::app::Context) -> anyhow::Result<ExitCode> {
        unimplemented!(
            "PackageSign::execute — Phase 5 resolves override token, calls \
             PackageManager::sign_one, and reports via api.report_signature"
        )
    }
}
