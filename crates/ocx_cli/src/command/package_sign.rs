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

use std::io::Read;
use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;
use ocx_lib::oci::sign::{SignError, SignErrorKind};

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
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifier = self.identifier.with_domain(context.default_registry())?;

        // S1-E policy: offline sign is a deliberate rejection, NOT a passive
        // network-access failure. Route through `SignErrorKind::OfflineSignRefused`
        // so the exit-code classifier returns 77 (PermissionDenied). This
        // short-circuits before we touch the token-resolution path: the
        // acceptance test `test_sign_offline_refused` drives this contract.
        if context.is_offline() {
            return Err(anyhow::Error::from(SignError::new(
                identifier,
                SignErrorKind::OfflineSignRefused,
            )));
        }

        // C-S1-4 token precedence: file > stdin > env. The resolved token is
        // a plain String; never log, never surface in error context.
        // Resolution itself is safe to run (no network, no crypto) — the token
        // will be consumed by Phase 5c's pipeline integration.
        let _override_token = self.resolve_override_token()?;

        // Phase 5c blocker: `SignPipeline::run` requires a `&dyn OciTransport`
        // (see `oci::sign::SignContext::transport`) but `oci::Client` keeps
        // its transport as a private field with no public accessor. Wiring
        // the pipeline call needs either a `Client::transport()` accessor or
        // a `SignContext::new_from_client()` helper — both are
        // design-change-shaped and belong to Phase 5c alongside the
        // sigstore-rs integration (the pipeline body itself is still
        // `unimplemented!()`). Until then we surface
        // `SignErrorKind::SigningPipelineInternal` so the exit-code classifier
        // produces `Failure` (1) rather than a panic.
        let blocker: Box<dyn std::error::Error + Send + Sync> =
            "SignPipeline::run is Phase 5c blocked on sigstore-rs integration + transport accessor".into();
        Err(anyhow::Error::from(SignError::new(
            identifier,
            SignErrorKind::SigningPipelineInternal(blocker),
        )))
    }

    /// Resolve the override OIDC token per C-S1-4 precedence.
    ///
    /// Precedence: `--identity-token-file` > `--identity-token-stdin` >
    /// `OCX_IDENTITY_TOKEN`. Returns `Ok(None)` when no override source
    /// supplies a token — the dispatcher then falls through to ambient
    /// detection or the browser path.
    ///
    /// The file and stdin paths trim trailing whitespace so a trailing newline
    /// written by `echo $TOKEN > tokenfile` doesn't poison the JWT.
    fn resolve_override_token(&self) -> anyhow::Result<Option<String>> {
        if let Some(path) = &self.identity_token_file {
            // Sync `std::fs::read_to_string` is fine here: tokens are small and
            // this is a CLI entry-path, not an async hot loop. Trim trailing
            // whitespace/newlines so `echo $TOKEN > tokenfile` doesn't poison
            // the JWT.
            let raw = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("failed to read --identity-token-file {}: {e}", path.display()))?;
            return Ok(Some(raw.trim().to_string()));
        }
        if self.identity_token_stdin {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| anyhow::anyhow!("failed to read identity token from stdin: {e}"))?;
            return Ok(Some(buf.trim().to_string()));
        }
        if let Ok(token) = std::env::var("OCX_IDENTITY_TOKEN")
            && !token.is_empty()
        {
            return Ok(Some(token));
        }
        Ok(None)
    }
}
