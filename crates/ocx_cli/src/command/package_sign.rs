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

use anyhow::Context as _;
use clap::Parser;
use tokio::io::AsyncReadExt;

use ocx_lib::oci;
use ocx_lib::oci::sign::{SignError, SignErrorKind};

use crate::command::sigstore_url::validate_sigstore_url;
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

        // SSRF hardening (CWE-918): validate user-supplied endpoint URLs at the
        // boundary before they become HTTP client targets.
        let _fulcio_url = validate_sigstore_url(&self.fulcio_url, "--fulcio-url")?;
        let _rekor_url = validate_sigstore_url(&self.rekor_url, "--rekor-url")?;

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
        let _override_token = self.resolve_override_token(&identifier).await?;

        // Phase 5c blocker: `SignPipeline::run` requires a `&dyn OciTransport`
        // (see `oci::sign::SignContext::transport`) but `oci::Client` keeps
        // its transport as a private field with no public accessor. Wiring
        // the pipeline call needs either a `Client::transport()` accessor or
        // a `SignContext::new_from_client()` helper — both are
        // design-change-shaped and belong to Phase 5c alongside the
        // sigstore-rs integration (the pipeline body itself is still
        // `unimplemented!()`). Until then we surface
        // `SignErrorKind::Internal` so the exit-code classifier
        // produces `Failure` (1) rather than a panic.
        let blocker: Box<dyn std::error::Error + Send + Sync> =
            "SignPipeline::run is Phase 5c blocked on sigstore-rs integration + transport accessor".into();
        Err(anyhow::Error::from(SignError::new(
            identifier,
            SignErrorKind::Internal(blocker),
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
    ///
    /// On Unix, the token file is rejected if any group or other permission
    /// bit is set (`mode & 0o077 != 0`). This enforces `chmod 600` hygiene:
    /// a world- or group-readable token file is a security misconfiguration
    /// and surfaces as [`SignErrorKind::IdentityTokenFilePermissive`] (exit 77).
    async fn resolve_override_token(&self, identifier: &oci::Identifier) -> anyhow::Result<Option<String>> {
        if let Some(path) = &self.identity_token_file {
            // On Windows, ACL-based permission validation is not implemented for
            // Slice 1 (windows-acl integration is out of scope). Refuse explicitly
            // rather than silently skipping the check — a readable-by-others token
            // file is a security misconfiguration and must not be accepted silently.
            #[cfg(windows)]
            {
                return Err(anyhow::Error::from(SignError::new(
                    identifier.clone(),
                    SignErrorKind::OidcPreCheckFailed {
                        reason: "identity-token-file permission validation is not supported on Windows; \
                                 use --identity-token-stdin or OCX_IDENTITY_TOKEN instead"
                            .into(),
                    },
                )));
            }
            // C-S1-4 permission gate (Unix only): open the file once, validate
            // permissions on the open handle, then read from the same handle to
            // eliminate the TOCTOU race between stat and read.
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let mut file = tokio::fs::File::open(path)
                    .await
                    .with_context(|| format!("failed to open --identity-token-file {}", path.display()))?;
                let meta = file
                    .metadata()
                    .await
                    .with_context(|| format!("failed to stat --identity-token-file {}", path.display()))?;
                let mode = meta.mode();
                if mode & 0o077 != 0 {
                    return Err(anyhow::Error::from(SignError::new(
                        identifier.clone(),
                        SignErrorKind::IdentityTokenFilePermissive {
                            path: path.clone(),
                            mode,
                        },
                    )));
                }
                let mut raw = String::new();
                file.read_to_string(&mut raw)
                    .await
                    .with_context(|| format!("failed to read --identity-token-file {}", path.display()))?;
                return Ok(Some(raw.trim().to_string()));
            }
            // Unreachable on non-unix, non-windows platforms (none currently supported).
            #[allow(unreachable_code)]
            return Ok(None);
        }
        if self.identity_token_stdin {
            // Use tokio's async stdin to avoid blocking the runtime thread.
            let mut buf = String::new();
            tokio::io::stdin()
                .read_to_string(&mut buf)
                .await
                .context("failed to read identity token from stdin")?;
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

#[cfg(test)]
mod tests {
    //! Unit tests for [`PackageSign::resolve_override_token`] — C-S1-4 contract.

    use std::str::FromStr;

    use super::*;
    use crate::options;

    fn test_identifier() -> oci::Identifier {
        oci::Identifier::parse("registry.example/pkg:1.0").expect("static parse")
    }

    fn make_sign_cmd(identity_token_file: Option<std::path::PathBuf>, identity_token_stdin: bool) -> PackageSign {
        PackageSign {
            platform: "linux/amd64".parse().expect("platform"),
            fulcio_url: "https://fulcio.sigstore.dev".into(),
            rekor_url: "https://rekor.sigstore.dev".into(),
            identity_token_file,
            identity_token_stdin,
            no_tty: false,
            no_cache: false,
            identifier: options::Identifier::from_str("registry.example/pkg:1.0").expect("ident"),
        }
    }

    /// Write `contents` to a new file in `dir` and set the given Unix mode.
    #[cfg(unix)]
    fn write_with_mode(dir: &std::path::Path, name: &str, contents: &str, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_file_0644_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_with_mode(tmp.path(), "token", "tok123\n", 0o644);
        let cmd = make_sign_cmd(Some(path), false);
        let id = test_identifier();
        let result = cmd.resolve_override_token(&id).await;
        let err = result.expect_err("0644 token file must be rejected");
        // Must classify as IdentityTokenFilePermissive via the error chain.
        let sign_err = err.downcast_ref::<SignError>().expect("SignError in chain");
        assert!(
            matches!(sign_err.kind, SignErrorKind::IdentityTokenFilePermissive { .. }),
            "expected IdentityTokenFilePermissive, got {:?}",
            sign_err.kind
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_file_0600_is_accepted() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_with_mode(tmp.path(), "token", "my-token\n", 0o600);
        let cmd = make_sign_cmd(Some(path), false);
        let id = test_identifier();
        let token = cmd.resolve_override_token(&id).await.expect("0600 must succeed");
        assert_eq!(token, Some("my-token".to_string()));
    }
}
